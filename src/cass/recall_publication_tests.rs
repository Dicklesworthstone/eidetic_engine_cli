//! Durable publication of recall evidence recovered after a source import.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;

use super::*;
use crate::core::index::{
    IndexHealth, IndexStatusOptions, IndexStatusReport, TestWorkspaceEmbedderStackGuard,
    get_index_status_with_connection, install_test_hash_workspace_embedder,
    process_pending_index_jobs_coalesced,
};
use crate::db::{AdvisoryLockId, SearchIndexJobStatus, StoredEvidenceSpan};

thread_local! {
    static BEFORE_RECORDER: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

pub(super) fn before_recorder() {
    let hook = BEFORE_RECORDER.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

struct Fixture {
    db: DbConnection,
    workspace_id: String,
    session_id: String,
    client: CassClient,
    options: CassImportOptions,
    index_dir: PathBuf,
    _embedder: TestWorkspaceEmbedderStackGuard,
    _root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace_path = root.path().join("workspace");
        std::fs::create_dir(&workspace_path).unwrap();
        let workspace_path = workspace_path.canonicalize().unwrap();
        let session_path = root.path().join("recall.jsonl");
        let failure = json!({"schema":"ee.error.v2","error":{
            "code":"migration_required", "message":"Schema migration required"
        }});
        let records = [
            json!({"type":"assistant", "content":"Starting the release checks."}),
            json!({"type":"assistant", "message":{"role":"assistant", "content":[{
                "type":"tool_use", "id":"failed", "name":"Bash",
                "input":{"command":"ee search release --json"}
            }]}}),
            json!({"type":"user", "message":{"role":"user", "content":[{
                "type":"tool_result", "tool_use_id":"failed", "is_error":true,
                "content":failure.to_string()
            }]}}),
            json!({"type":"assistant", "message":{"role":"assistant", "content":[{
                "type":"text", "text":"Updated the workspace schema by applying the pending migrations, which fixes opening the evidence tables during search."
            }]}}),
            json!({"type":"assistant", "message":{"role":"assistant", "content":[{
                "type":"tool_use", "id":"verified", "name":"Bash",
                "input":{"command":"ee search release --json"}
            }]}}),
            json!({"type":"user", "message":{"role":"user", "content":[{
                "type":"tool_result", "tool_use_id":"verified", "is_error":false,
                "content":r#"{"schema":"ee.response.v2","success":true,"data":{"results":[]}}"#
            }]}}),
        ];
        let source = records
            .iter()
            .map(JsonValue::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&session_path, source).unwrap();
        let sessions = json!({"sessions":[{
            "path":session_path, "workspace":workspace_path, "agent":"codex",
            "modified":"2026-05-05T00:00:00Z", "message_count":records.len()
        }]});
        let lines = records
            .iter()
            .enumerate()
            .map(|(index, record)| json!({"line":index + 1, "content":record.to_string()}))
            .collect::<Vec<_>>();
        let view = serde_json::to_string_pretty(&json!({
            "path":session_path, "lines":lines, "total_lines":records.len()
        }))
        .unwrap();
        let binary = root.path().join("cass");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\nsessions) cat <<'EE_SESSIONS'\n{sessions}\nEE_SESSIONS\n;;\nview) cat <<'EE_VIEW'\n{view}\nEE_VIEW\n;;\n*) exit 2;;\nesac\n"
        );
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let database_path = root.path().join("ee.db");
        let db = DbConnection::open_file(&database_path).unwrap();
        db.migrate().unwrap();
        let workspace_id = ensure_workspace(&db, &workspace_path).unwrap();
        let session_id = stable_session_id(&workspace_id, &session_path.to_string_lossy());
        let embedder = install_test_hash_workspace_embedder(&workspace_id);
        let options = CassImportOptions {
            workspace_path,
            database_path: Some(database_path),
            limit: 1,
            since: None,
            dry_run: false,
            include_spans: true,
        };
        Self {
            db,
            workspace_id,
            session_id,
            client: CassClient::with_binary(binary).with_timeout(Duration::from_secs(5)),
            options,
            index_dir: root.path().join("index"),
            _embedder: embedder,
            _root: root,
        }
    }

    fn import(&self, include_spans: bool) -> CassImportReport {
        let mut options = self.options.clone();
        options.include_spans = include_spans;
        import_cass_sessions(&self.client, &options).unwrap()
    }

    fn import_source_without_derivation(&self) -> CassImportReport {
        self.db.execute_raw(
            "CREATE TRIGGER reject_recall BEFORE INSERT ON error_fingerprints BEGIN SELECT RAISE(ABORT, 'recall_fixture_failure'); END;"
        ).unwrap();
        let report = self.import(true);
        self.db.execute_raw("DROP TRIGGER reject_recall").unwrap();
        assert_eq!(report.sessions_imported, 1);
        assert_eq!(report.spans_imported, 6);
        assert_eq!(report.index_jobs_queued, 1);
        assert_eq!(report.sessions[0].index_job_id, Some(self.source_job()));
        assert!(self.db.get_audit(&self.marker_id()).unwrap().is_none());
        assert!(self.cards().is_empty());
        report
    }

    fn source_job(&self) -> String {
        refresh::committed_session_publication_id(&self.db, &self.workspace_id, &self.session_id)
            .unwrap()
    }

    fn marker_id(&self) -> String {
        stable_cass_error_recall_snapshot_audit_id(&self.session_id, &self.source_job())
    }

    fn derived_job(&self) -> String {
        stable_cass_error_recall_index_job_id(&self.marker_id())
    }

    fn generation(&self) -> Option<u64> {
        self.db
            .get_workspace_generation(&self.workspace_id)
            .unwrap()
    }

    fn cards(&self) -> Vec<StoredEvidenceSpan> {
        self.db
            .list_evidence_spans_for_session(&self.session_id)
            .unwrap()
            .into_iter()
            .filter(StoredEvidenceSpan::is_derived_incident_card)
            .collect()
    }

    fn status(&self) -> IndexStatusReport {
        get_index_status_with_connection(
            &IndexStatusOptions {
                workspace_path: self.options.workspace_path.clone(),
                database_path: self.options.database_path.clone(),
                index_dir: Some(self.index_dir.clone()),
            },
            Some(&self.db),
        )
        .unwrap()
    }

    fn publish(&self) {
        let reports = process_pending_index_jobs_coalesced(
            &self.db,
            &self.workspace_id,
            &self.index_dir,
            None,
        )
        .unwrap();
        assert!(!reports.is_empty());
        assert!(
            reports.iter().all(|report| report.outcome == "completed"),
            "{reports:?}"
        );
        let status = self.status();
        assert_eq!(status.health, IndexHealth::Ready, "{status:?}");
        assert_eq!(status.db_generation, status.index_generation);
    }

    fn indexed_ids(&self) -> BTreeSet<String> {
        let index =
            frankensearch::VectorIndex::open_read_only(&self.index_dir.join("vector.fast.idx"))
                .unwrap();
        (0..index.record_count())
            .map(|position| index.doc_id_at(position).unwrap().to_owned())
            .collect()
    }

    fn assert_card_published(&self, card_id: &str) {
        assert!(self.indexed_ids().contains(card_id));
        #[cfg(feature = "lexical-bm25")]
        {
            use crate::search::LexicalRead;
            let lexical =
                crate::search::TantivyIndex::open_read_only(&self.index_dir.join("lexical"))
                    .unwrap();
            let hits = crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
                lexical.search(&cx, "migration", 128).await
            })
            .unwrap()
            .unwrap();
            assert!(hits.iter().any(|hit| hit.doc_id == card_id), "{hits:?}");
        }
    }
}

#[test]
fn unchanged_import_refreshes_old_derivation_and_publishes_one_immutable_revision() {
    let fixture = Fixture::new();
    fixture.import_source_without_derivation();
    let source_job = fixture.source_job();
    let session = fixture
        .db
        .get_session(&fixture.session_id)
        .unwrap()
        .unwrap();
    let sources = fixture
        .db
        .list_evidence_spans_for_session(&fixture.session_id)
        .unwrap();
    let arcs = crate::core::cass_error_recall::session_failure_arcs(
        &fixture.workspace_id,
        &session,
        &sources,
    );
    assert_eq!(arcs.len(), 1);
    let current = crate::core::incident_card::draft_incident_card(
        &fixture.workspace_id,
        &fixture.session_id,
        &sources,
        &arcs[0],
    )
    .unwrap();
    let revision = current.revision_draft();
    let mut previous = current.input.clone();
    previous.excerpt.push_str("\nEarlier incident rendering.");
    previous.content_hash = format!(
        "blake3:{}",
        blake3::hash(previous.excerpt.as_bytes()).to_hex()
    );
    fixture
        .db
        .insert_evidence_span(&current.id, &previous)
        .unwrap();
    let old_card = fixture.db.get_evidence_span(&current.id).unwrap().unwrap();
    // Reproduce the durable marker from the prior extractor revision. The
    // transcript and its publication identity stay unchanged during retry.
    let old_derivation = AuditId::from_uuid(stable_uuid(&format!(
        "audit:{}:cass_error_recall.v19:{}:{}",
        crate::db::audit_actions::CASS_ERROR_RECALL_DERIVE,
        crate::core::incident_card::INCIDENT_CARD_DERIVATION,
        fixture.session_id,
    )))
    .to_string();
    let old_marker_id = AuditId::from_uuid(stable_uuid(&format!(
        "audit:cass-error-recall-snapshot:{old_derivation}:{source_job}"
    )))
    .to_string();
    fixture
        .db
        .insert_audit(
            &old_marker_id,
            &CreateAuditInput {
                workspace_id: Some(fixture.workspace_id.clone()),
                actor: Some(crate::core::cass_error_recall::CASS_ERROR_RECALL_ACTOR.to_owned()),
                action: crate::db::audit_actions::CASS_ERROR_RECALL_DERIVE.to_owned(),
                target_type: Some("session".to_owned()),
                target_id: Some(fixture.session_id.clone()),
                details: Some(
                    json!({
                        "committedPublicationId":source_job,
                        "fingerprintsRecorded":0,
                        "repairLinksRecorded":0,
                        "incidentCardsRecorded":1,
                        "errorRecallDerivation":"cass_error_recall.v19",
                    })
                    .to_string(),
                ),
            },
        )
        .unwrap();
    let old_marker = fixture.db.get_audit(&old_marker_id).unwrap();
    fixture.publish();
    fixture.assert_card_published(&old_card.id);

    let retry = fixture.import(false);
    assert_eq!(retry.index_jobs_queued, 1);
    assert_eq!(retry.sessions[0].index_job_id, Some(fixture.derived_job()));
    assert_eq!(fixture.source_job(), source_job);
    assert_eq!(fixture.cards().len(), 2);
    assert_eq!(
        fixture.db.get_evidence_span(&old_card.id).unwrap().as_ref(),
        Some(&old_card)
    );
    assert_eq!(fixture.db.get_audit(&old_marker_id).unwrap(), old_marker);
    let new_card = fixture.db.get_evidence_span(&revision.id).unwrap().unwrap();
    assert!(new_card.matches_screened_input(&revision.input).unwrap());
    let marker = fixture.db.get_audit(&fixture.marker_id()).unwrap().unwrap();
    let details: JsonValue = serde_json::from_str(marker.details.as_deref().unwrap()).unwrap();
    assert_eq!(details["incidentCardsRecorded"], 1);
    assert_eq!(
        details["errorRecallDerivation"],
        crate::core::cass_error_recall::CASS_ERROR_RECALL_DERIVATION
    );
    fixture.publish();
    fixture.assert_card_published(&new_card.id);
    let mut checks = crate::core::incident_card::proof::CardChecks::default();
    checks.observe(&old_card).unwrap();
    checks.observe(&new_card).unwrap();
    assert_eq!(
        checks.withheld(&fixture.db, &fixture.workspace_id).unwrap(),
        BTreeSet::from([old_card.id])
    );

    let generation = fixture.generation();
    assert_eq!(fixture.import(false).index_jobs_queued, 0);
    assert_eq!(fixture.generation(), generation);
    assert_eq!(fixture.cards().len(), 2);
}

#[test]
fn completed_source_publication_does_not_hide_recovered_recall_evidence() {
    for include_spans in [true, false] {
        let fixture = Fixture::new();
        fixture.import_source_without_derivation();
        fixture.publish();
        let source_job = fixture.source_job();
        let completed_source = fixture.db.get_search_index_job(&source_job).unwrap();
        let session = fixture.db.get_session(&fixture.session_id).unwrap();
        let retry = fixture.import(include_spans);
        assert_eq!(retry.sessions_imported, 0);
        assert_eq!(retry.sessions_skipped, 1);
        assert_eq!(retry.spans_imported, 0);
        assert_eq!(retry.index_jobs_queued, 1);
        assert_eq!(retry.sessions[0].index_job_id, Some(fixture.derived_job()));
        assert_ne!(fixture.derived_job(), source_job);
        assert_eq!(fixture.source_job(), source_job);
        assert_eq!(
            fixture.db.get_search_index_job(&source_job).unwrap(),
            completed_source
        );
        assert_eq!(
            fixture.db.get_session(&fixture.session_id).unwrap(),
            session
        );
        let cards = fixture.cards();
        assert_eq!(cards.len(), 1);
        assert!(!fixture.indexed_ids().contains(&cards[0].id));
        assert_eq!(fixture.status().health, IndexHealth::Stale);
        fixture.publish();
        fixture.assert_card_published(&cards[0].id);
        let generation = fixture.generation();
        let marker = fixture.db.get_audit(&fixture.marker_id()).unwrap();
        let repeated = fixture.import(include_spans);
        assert_eq!(repeated.index_jobs_queued, 0);
        assert!(repeated.sessions[0].index_job_id.is_none());
        assert_eq!(fixture.generation(), generation);
        assert_eq!(fixture.cards(), cards);
        assert_eq!(fixture.db.get_audit(&fixture.marker_id()).unwrap(), marker);
        assert_eq!(fixture.db.count_table_rows("search_index_jobs").unwrap(), 2);
    }
}

#[test]
fn failed_marker_or_job_commit_recovers_cards_without_counting_them_twice() {
    for failure in ["marker", "job"] {
        let fixture = Fixture::new();
        fixture.import_source_without_derivation();
        fixture.publish();
        let trigger = if failure == "marker" {
            "CREATE TRIGGER reject_recall_completion BEFORE INSERT ON audit_log WHEN NEW.action = 'cass.error_recall_derive.v1' BEGIN SELECT RAISE(ABORT, 'recall_marker_failure'); END;".to_owned()
        } else {
            format!(
                "CREATE TRIGGER reject_recall_completion BEFORE INSERT ON search_index_jobs WHEN NEW.id = '{}' BEGIN SELECT RAISE(ABORT, 'recall_job_failure'); END;",
                fixture.derived_job()
            )
        };
        fixture.db.execute_raw(&trigger).unwrap();
        let interrupted = fixture.import(true);
        assert_eq!(interrupted.sessions_skipped, 1);
        assert_eq!(interrupted.index_jobs_queued, 0);
        assert!(
            fixture
                .db
                .get_audit(&fixture.marker_id())
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .db
                .get_search_index_job(&fixture.derived_job())
                .unwrap()
                .is_none()
        );
        assert_eq!(fixture.db.count_table_rows("search_index_jobs").unwrap(), 1);
        let cards = fixture.cards();
        assert_eq!(cards.len(), 1, "the recorder's own transaction committed");
        fixture
            .db
            .execute_raw("DROP TRIGGER reject_recall_completion")
            .unwrap();
        let recovered = fixture.import(false);
        assert_eq!(recovered.index_jobs_queued, 1);
        assert_eq!(
            recovered.sessions[0].index_job_id,
            Some(fixture.derived_job())
        );
        assert_eq!(fixture.cards(), cards);
        let marker = fixture.db.get_audit(&fixture.marker_id()).unwrap().unwrap();
        let details: JsonValue = serde_json::from_str(marker.details.as_deref().unwrap()).unwrap();
        assert_eq!(details["incidentCardsRecorded"], 0);
        assert!(details["fingerprintsRecorded"].as_u64().unwrap() > 0);
        fixture.publish();
        fixture.assert_card_published(&cards[0].id);
        let generation = fixture.generation();
        assert_eq!(fixture.import(true).index_jobs_queued, 0);
        assert_eq!(fixture.generation(), generation);
        assert_eq!(fixture.db.count_table_rows("search_index_jobs").unwrap(), 2);
    }
}

#[test]
fn existing_snapshot_marker_recovers_its_missing_publication_without_rederiving() {
    let fixture = Fixture::new();
    fixture.import_source_without_derivation();
    fixture.publish();
    let report = crate::core::cass_error_recall::record_session_error_recall(
        &fixture.db,
        &fixture.workspace_id,
        &fixture.session_id,
    )
    .unwrap();
    // This is the marker shape written before recall publication was queued.
    fixture
        .db
        .insert_audit(
            &fixture.marker_id(),
            &CreateAuditInput {
                workspace_id: Some(fixture.workspace_id.clone()),
                actor: Some(crate::core::cass_error_recall::CASS_ERROR_RECALL_ACTOR.to_owned()),
                action: crate::db::audit_actions::CASS_ERROR_RECALL_DERIVE.to_owned(),
                target_type: Some("session".to_owned()),
                target_id: Some(fixture.session_id.clone()),
                details: Some(
                    json!({
                        "committedPublicationId":fixture.source_job(),
                        "fingerprintsRecorded":report.fingerprints_recorded,
                        "repairLinksRecorded":report.repair_links_recorded,
                        "incidentCardsRecorded":report.incident_cards_recorded,
                    })
                    .to_string(),
                ),
            },
        )
        .unwrap();
    let generation = fixture.generation();
    let marker = fixture.db.get_audit(&fixture.marker_id()).unwrap();
    let cards = fixture.cards();
    assert_eq!(cards.len(), 1);
    assert!(
        fixture
            .db
            .get_search_index_job(&fixture.derived_job())
            .unwrap()
            .is_none()
    );
    let retry = fixture.import(false);
    assert_eq!(retry.index_jobs_queued, 1);
    assert_eq!(retry.sessions[0].index_job_id, Some(fixture.derived_job()));
    assert_eq!(fixture.generation(), generation);
    assert_eq!(fixture.db.get_audit(&fixture.marker_id()).unwrap(), marker);
    fixture.publish();
    fixture.assert_card_published(&cards[0].id);
    assert_eq!(fixture.import(false).index_jobs_queued, 0);
    assert_eq!(fixture.generation(), generation);
}

#[test]
fn derived_retries_preserve_live_publishers_and_reuse_failed_or_cancelled_work() {
    let fixture = Fixture::new();
    fixture.import_source_without_derivation();
    let source_job = fixture.source_job();
    assert!(fixture.db.start_search_index_job(&source_job).unwrap());
    let lock = AdvisoryLockId::index(&fixture.workspace_id);
    let holder = format!("index:{}:recall-publication", std::process::id());
    assert!(
        fixture
            .db
            .acquire_advisory_lock(&lock, &holder, None, Some("fixture"))
            .unwrap()
            .is_acquired()
    );
    let lease = fixture.db.list_active_advisory_locks().unwrap();
    let source = fixture.db.get_search_index_job(&source_job).unwrap();
    let retry = fixture.import(false);
    let derived_job = fixture.derived_job();
    assert_eq!(retry.index_jobs_queued, 1);
    assert_eq!(retry.sessions[0].index_job_id, Some(derived_job.clone()));
    assert_eq!(
        fixture.db.get_search_index_job(&source_job).unwrap(),
        source
    );
    assert_eq!(fixture.db.list_active_advisory_locks().unwrap(), lease);
    let generation = fixture.generation();
    assert!(fixture.db.start_search_index_job(&derived_job).unwrap());
    for state in [
        SearchIndexJobStatus::Running,
        SearchIndexJobStatus::Failed,
        SearchIndexJobStatus::Cancelled,
    ] {
        match state {
            SearchIndexJobStatus::Failed => {
                assert!(
                    fixture
                        .db
                        .fail_search_index_job(&derived_job, "fixture failure")
                        .unwrap()
                );
            }
            SearchIndexJobStatus::Cancelled => {
                assert!(
                    fixture
                        .db
                        .requeue_search_index_job_for_retry(&derived_job)
                        .unwrap()
                );
                assert!(fixture.db.cancel_search_index_job(&derived_job).unwrap());
            }
            _ => {}
        }
        let before = fixture.db.get_search_index_job(&derived_job).unwrap();
        assert_eq!(before.as_ref().unwrap().status_enum(), Some(state));
        let retry = fixture.import(true);
        assert_eq!(retry.index_jobs_queued, 1);
        assert_eq!(retry.sessions[0].index_job_id, Some(derived_job.clone()));
        assert_eq!(
            fixture.db.get_search_index_job(&derived_job).unwrap(),
            before
        );
        assert_eq!(
            fixture.db.get_search_index_job(&source_job).unwrap(),
            source
        );
        assert_eq!(fixture.db.list_active_advisory_locks().unwrap(), lease);
        assert_eq!(fixture.generation(), generation);
        assert_eq!(fixture.db.count_table_rows("search_index_jobs").unwrap(), 2);
    }
    assert!(fixture.db.release_advisory_lock(&lock, &holder).unwrap());
    fixture.publish();
    fixture.assert_card_published(&fixture.cards()[0].id);
    assert_eq!(fixture.import(false).index_jobs_queued, 0);
}

#[test]
fn empty_derivation_does_not_queue_extra_publication() {
    let fixture = Fixture::new();
    let first = fixture.import(false);
    assert_eq!(first.index_jobs_queued, 1);
    assert_eq!(first.sessions[0].index_job_id, Some(fixture.source_job()));
    assert!(
        fixture
            .db
            .get_audit(&fixture.marker_id())
            .unwrap()
            .is_some()
    );
    assert!(
        fixture
            .db
            .get_search_index_job(&fixture.derived_job())
            .unwrap()
            .is_none()
    );
    fixture.publish();
    let generation = fixture.generation();
    assert_eq!(fixture.import(false).index_jobs_queued, 0);
    assert_eq!(fixture.generation(), generation);
    assert_eq!(fixture.db.count_table_rows("search_index_jobs").unwrap(), 1);
}

#[test]
fn overlapping_derivers_check_the_marker_inside_the_same_writer_fence() {
    let fixture = Fixture::new();
    fixture.import_source_without_derivation();
    fixture.publish();
    let (opened_tx, opened_rx) = mpsc::channel();
    let (owner_start_tx, owner_start_rx) = mpsc::channel();
    let (contender_start_tx, contender_start_rx) = mpsc::channel();
    let (owner_entered_tx, owner_entered_rx) = mpsc::channel();
    let (owner_release_tx, owner_release_rx) = mpsc::channel();
    let (contender_entered_tx, contender_entered_rx) = mpsc::channel();
    let spawn = |start: mpsc::Receiver<()>, hook: Box<dyn FnOnce() + Send>| {
        let database = fixture.options.database_path.clone().unwrap();
        let workspace = fixture.workspace_id.clone();
        let session = fixture.session_id.clone();
        let opened = opened_tx.clone();
        std::thread::spawn(move || {
            let db = DbConnection::open_file(&database).unwrap();
            BEFORE_RECORDER.with(|slot| *slot.borrow_mut() = Some(hook));
            opened.send(()).unwrap();
            start.recv_timeout(Duration::from_secs(10)).unwrap();
            let job = derive_session_error_recall(&db, &workspace, &session);
            (job, db.get_workspace_generation(&workspace).unwrap())
        })
    };
    let owner = spawn(
        owner_start_rx,
        Box::new(move || {
            owner_entered_tx.send(()).unwrap();
            owner_release_rx
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }),
    );
    let contender = spawn(
        contender_start_rx,
        Box::new(move || {
            contender_entered_tx.send(()).unwrap();
        }),
    );
    // Both opens complete before the owner takes the reentrant fence, since
    // opening a writable file also uses that fence.
    opened_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    opened_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    owner_start_tx.send(()).unwrap();
    owner_entered_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    contender_start_tx.send(()).unwrap();
    let contender_passed_guard = contender_entered_rx.recv_timeout(Duration::from_millis(200));
    owner_release_tx.send(()).unwrap();
    let owner = owner.join().unwrap();
    let contender = contender.join().unwrap();
    assert!(
        matches!(contender_passed_guard, Err(mpsc::RecvTimeoutError::Timeout)),
        "a competing derivation passed the missing-marker guard while the owner was deriving"
    );
    assert_eq!(owner.0, Some(fixture.derived_job()));
    assert_eq!(contender.0, owner.0);
    assert_eq!(
        contender.1, owner.1,
        "the second importer must not replay source upserts"
    );
    assert_eq!(fixture.generation(), owner.1);
    assert_eq!(fixture.cards().len(), 1);
    assert_eq!(fixture.db.count_table_rows("search_index_jobs").unwrap(), 2);
    fixture.publish();
    let generation = fixture.generation();
    assert!(
        derive_session_error_recall(&fixture.db, &fixture.workspace_id, &fixture.session_id)
            .is_none()
    );
    assert_eq!(fixture.generation(), generation);
}
