//! Exercise streamed capture through real source rows and staged publication.

use super::super::*;
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

struct Fixture {
    db: DbConnection,
    workspace_id: String,
    index_dir: PathBuf,
    _embedder_guard: TestWorkspaceEmbedderStackGuard,
    root: tempfile::TempDir,
}

impl Fixture {
    fn new(memories: u32) -> Self {
        let root = tempfile::tempdir().expect("source fixture directory");
        let db = DbConnection::open_file(&root.path().join("source.db")).expect("source database");
        db.migrate().expect("source schema");
        let workspace_id = crate::models::WorkspaceId::from_uuid(uuid::Uuid::new_v4()).to_string();
        db.insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: root
                    .path()
                    .canonicalize()
                    .expect("workspace path")
                    .display()
                    .to_string(),
                name: Some("streamed source publication".to_owned()),
            },
        )
        .expect("workspace");
        let embedder_guard = install_test_hash_workspace_embedder(&workspace_id);
        db.with_transaction(|| {
            for number in 0..memories {
                insert_memory(
                    &db,
                    &workspace_id,
                    &memory_id(number),
                    &format!("Stable source memory number {number}."),
                    Vec::new(),
                )?;
            }
            Ok(())
        })
        .expect("seed source rows");
        Self {
            db,
            workspace_id,
            index_dir: root.path().join("index"),
            _embedder_guard: embedder_guard,
            root,
        }
    }

    fn other_workspace(&self) -> String {
        let id = crate::models::WorkspaceId::from_uuid(uuid::Uuid::new_v4()).to_string();
        self.db
            .insert_workspace(
                &id,
                &CreateWorkspaceInput {
                    path: self
                        .root
                        .path()
                        .join("other-workspace")
                        .display()
                        .to_string(),
                    name: Some("foreign source scope".to_owned()),
                },
            )
            .expect("foreign workspace");
        id
    }

    fn queue(&self, ordinal: u32, document_id: &str) -> StoredSearchIndexJob {
        let id = format!("sidx_{ordinal:026}");
        self.db
            .insert_search_index_job(
                &id,
                &CreateSearchIndexJobInput {
                    workspace_id: self.workspace_id.clone(),
                    job_type: SearchIndexJobType::SingleDocument,
                    document_source: Some("memory".to_owned()),
                    document_id: Some(document_id.to_owned()),
                    documents_total: 1,
                },
            )
            .expect("queued source job");
        self.db
            .get_search_index_job(&id)
            .expect("job lookup")
            .expect("queued job")
    }

    fn process(&self, job: &StoredSearchIndexJob) -> IndexProcessingJobReport {
        let report = process_index_job_for_connection(&self.db, &job.id, &self.index_dir)
            .expect("source job processor");
        assert_eq!(report.outcome, "completed", "{report:?}");
        report
    }

    fn seed_index(&self) {
        self.process(&self.queue(0, &memory_id(0)));
        self.assert_published(&self.snapshot());
    }

    fn snapshot(&self) -> WorkspaceIndexSourceSnapshot {
        collect_workspace_index_source_snapshot(&self.db, &self.workspace_id)
            .expect("complete canonical source snapshot")
    }

    fn streamed(&self) -> Option<super::SourceDeltaSnapshot> {
        let reconciler =
            delta::SourceReconciler::read(&self.index_dir, &hash_fallback_embedder_stack())
                .expect("compatible published digests");
        super::capture(
            &asupersync::Cx::for_testing(),
            &self.db,
            &self.workspace_id,
            reconciler,
        )
        .expect("streamed source snapshot")
    }

    fn assert_published(&self, snapshot: &WorkspaceIndexSourceSnapshot) {
        let published: serde_json::Value = serde_json::from_slice(
            &std::fs::read(self.index_dir.join(delta::DOC_DIGESTS_FILE))
                .expect("published digests"),
        )
        .expect("digest JSON");
        assert_eq!(published, canonical_digests(snapshot));
        assert_eq!(
            read_index_metadata(&self.index_dir).generation,
            Some(snapshot.generation)
        );
        assert_eq!(
            vector_rows(&self.index_dir)
                .into_keys()
                .collect::<BTreeSet<_>>(),
            snapshot
                .documents
                .iter()
                .map(|document| document.id.clone())
                .collect::<BTreeSet<_>>(),
        );
    }
}

fn memory_id(ordinal: u32) -> String {
    format!("mem_{ordinal:026}")
}

fn insert_memory(
    db: &DbConnection,
    workspace_id: &str,
    id: &str,
    content: &str,
    tags: Vec<String>,
) -> Result<(), DbError> {
    db.insert_memory(
        id,
        &CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: Some("test://streamed-index-source".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags,
            valid_from: None,
            valid_to: None,
        },
    )
}

fn canonical_digests(snapshot: &WorkspaceIndexSourceSnapshot) -> serde_json::Value {
    serde_json::to_value(delta::DocDigests::new(
        &hash_fallback_embedder_stack(),
        &snapshot.documents,
    ))
    .expect("complete canonical digests")
}

fn vector_rows(index_dir: &Path) -> BTreeMap<String, Vec<u32>> {
    let index =
        frankensearch::TwoTierIndex::open(index_dir, frankensearch::TwoTierConfig::default())
            .expect("native published vector index");
    (0..index.doc_count())
        .map(|position| {
            let id = index
                .doc_id_at(position)
                .expect("native vector ID")
                .to_owned();
            let values = index
                .fast_vector_for_doc_id(&id)
                .expect("native vector lookup")
                .expect("fast vector exists")
                .iter()
                .map(|value| value.to_bits())
                .collect();
            (id, values)
        })
        .collect()
}

fn assert_delta(report: &IndexProcessingJobReport) {
    assert_eq!(report.outcome, "completed", "{report:?}");
    assert!(
        report.processing_mode.ends_with("_staged_delta"),
        "{report:?}"
    );
    assert_eq!(report.fallback_to_full, None, "{report:?}");
}

#[test]
fn streamed_publication_reconciles_unqueued_writes_and_global_withdrawals() {
    let fixture = Fixture::new(16);
    let foreign = fixture.other_workspace();
    let global = memory_id(900);
    insert_memory(
        &fixture.db,
        &foreign,
        &global,
        "A globally scoped source that will become private.",
        vec![crate::models::GLOBAL_MEMORY_SCOPE_TAG.to_owned()],
    )
    .expect("global memory");
    fixture.seed_index();
    let baseline = vector_rows(&fixture.index_dir);
    assert!(baseline.contains_key(&global));

    let generation = fixture
        .db
        .get_workspace_generation(&fixture.workspace_id)
        .expect("generation");
    fixture
        .db
        .remove_memory_tags(
            &global,
            &[crate::models::GLOBAL_MEMORY_SCOPE_TAG.to_owned()],
        )
        .expect("withdraw foreign global scope");
    assert_eq!(
        fixture
            .db
            .get_workspace_generation(&fixture.workspace_id)
            .expect("local generation"),
        generation,
        "a foreign scope change must still be reconciled without a local generation bump",
    );
    fixture.db.execute_raw(&format!(
        "UPDATE memories SET content = 'The claimed source now describes an atomic repair.' WHERE id = '{}'",
        memory_id(0),
    )).expect("claimed raw mutation");
    fixture.db.execute_raw(&format!(
        "UPDATE memories SET content = 'An unqueued sibling now describes recovery from interruption.' WHERE id = '{}'",
        memory_id(1),
    )).expect("unqueued raw mutation");
    let job = fixture.queue(1, &memory_id(0));
    let expected = fixture.snapshot();
    let streamed = fixture.streamed().expect("small complete-source delta");
    assert_eq!(streamed.reconciliation.delta.upserts.len(), 2);
    assert_eq!(streamed.reconciliation.delta.removals, vec![global.clone()]);
    assert_eq!(
        serde_json::to_value(streamed.reconciliation.digests).expect("stream digests"),
        canonical_digests(&expected)
    );

    assert_delta(&fixture.process(&job));
    fixture.assert_published(&expected);
    let current = vector_rows(&fixture.index_dir);
    assert!(!current.contains_key(&global));
    assert_ne!(current[&memory_id(0)], baseline[&memory_id(0)]);
    assert_ne!(current[&memory_id(1)], baseline[&memory_id(1)]);
    assert_eq!(current[&memory_id(2)], baseline[&memory_id(2)]);
}

#[test]
fn streamed_stage_keeps_captured_bodies_and_generation_after_snapshot_commit() {
    let fixture = Fixture::new(16);
    fixture.seed_index();
    fixture.db.execute_raw(&format!(
        "UPDATE memories SET content = 'The captured source describes durable staging.' WHERE id = '{}'",
        memory_id(0),
    )).expect("captured mutation");
    let job = fixture.queue(1, &memory_id(0));
    let expected = fixture.snapshot();
    let reference = fixture.root.path().join("captured-reference");
    let stats = build_index_generation_sync(
        &reference,
        hash_fallback_embedder_stack(),
        expected.documents.clone(),
    )
    .expect("fresh vectors for the captured source bodies");
    validate_built_generation(&reference, stats, expected.document_counts)
        .expect("complete captured reference generation");
    let captured_vectors = vector_rows(&reference);
    assert_eq!(
        fixture
            .streamed()
            .expect("small delta")
            .reconciliation
            .delta
            .upserts
            .len(),
        1
    );
    let late_memory = memory_id(16);
    let report = process_one_index_job_after_snapshot(&fixture.db, &job, &fixture.index_dir, || {
        fixture.db.execute_raw(&format!(
            "UPDATE memories SET content = 'A later source describes a different recovery protocol.' WHERE id = '{}'",
            memory_id(0),
        ))?;
        insert_memory(&fixture.db, &fixture.workspace_id, &late_memory, "A new source committed after staging.", Vec::new())?;
        fixture.queue(2, &late_memory);
        Ok(())
    }).expect("publish captured stage");
    assert_delta(&report);
    fixture.assert_published(&expected);
    assert_eq!(vector_rows(&fixture.index_dir), captured_vectors);
    let current = fixture.snapshot();
    assert!(current.generation > expected.generation);
    assert_ne!(canonical_digests(&current), canonical_digests(&expected));
    assert!(!vector_rows(&fixture.index_dir).contains_key(&late_memory));
    let late_job = fixture
        .db
        .get_search_index_job(&format!("sidx_{:026}", 2))
        .expect("late job")
        .expect("queued late job");
    assert_eq!(late_job.status_enum(), Some(SearchIndexJobStatus::Pending));
    assert_delta(&fixture.process(&late_job));
    fixture.assert_published(&current);
}

#[test]
fn coalesced_streamed_stage_covers_unclaimed_jobs_and_fences_late_writes() {
    let fixture = Fixture::new(16);
    fixture.seed_index();
    let baseline = vector_rows(&fixture.index_dir);
    let jobs = (0..3)
        .map(|number| {
            fixture
                .db
                .execute_raw(&format!(
                    "UPDATE memories SET content = 'Coalesced source {number} now records a recovered publication.' WHERE id = '{}'",
                    memory_id(number),
                ))
                .expect("coalesced source mutation");
            fixture.queue(number + 1, &memory_id(number))
        })
        .collect::<Vec<_>>();
    let expected = fixture.snapshot();
    assert_eq!(
        fixture
            .streamed()
            .expect("three inputs fit streamed delta")
            .reconciliation
            .delta
            .upserts
            .len(),
        3,
    );
    let late_memory = memory_id(16);
    let late_job_id = format!("sidx_{:026}", 4);
    let reports = process_pending_index_jobs_coalesced_after_snapshot(
        &fixture.db,
        &fixture.workspace_id,
        &fixture.index_dir,
        Some(2),
        || {
            insert_memory(
                &fixture.db,
                &fixture.workspace_id,
                &late_memory,
                "A source committed after the shared stage was captured.",
                Vec::new(),
            )?;
            fixture.queue(4, &late_memory);
            Ok(())
        },
    )
    .expect("coalesced streamed publication");
    assert_eq!(reports.len(), 2);
    for (report, job) in reports.iter().zip(&jobs) {
        assert_delta(report);
        assert_eq!(report.job_id, job.id);
        assert!(
            report
                .processing_mode
                .contains("_open_sibling_full_rebuild")
        );
        assert_eq!(report.documents_total, expected.documents_total);
        assert_eq!(report.documents_indexed, expected.documents_total);
        assert_eq!(
            fixture
                .db
                .get_search_index_job(&job.id)
                .expect("claimed job")
                .expect("completed row")
                .status_enum(),
            Some(SearchIndexJobStatus::Completed),
        );
    }
    fixture.assert_published(&expected);
    let published = vector_rows(&fixture.index_dir);
    assert_ne!(
        published[&memory_id(2)],
        baseline[&memory_id(2)],
        "the unclaimed committed source belongs to the captured stage"
    );
    assert!(!published.contains_key(&late_memory));
    for job_id in [&jobs[2].id, &late_job_id] {
        assert_eq!(
            fixture
                .db
                .get_search_index_job(job_id)
                .expect("pending lookup")
                .expect("pending row")
                .status_enum(),
            Some(SearchIndexJobStatus::Pending),
        );
    }
    let current = fixture.snapshot();
    assert!(current.generation > expected.generation);
    let drained = process_pending_index_jobs_coalesced(
        &fixture.db,
        &fixture.workspace_id,
        &fixture.index_dir,
        None,
    )
    .expect("coalesced remaining jobs");
    assert_eq!(drained.len(), 2);
    for report in &drained {
        assert_delta(report);
        assert_eq!(report.documents_indexed, current.documents_total);
    }
    fixture.assert_published(&current);
    assert!(
        fixture
            .db
            .list_pending_search_index_jobs(&fixture.workspace_id, None)
            .expect("drained queue")
            .is_empty()
    );
}

#[test]
fn streamed_bulk_metadata_does_not_require_complete_source_fallback() {
    let fixture = Fixture::new(257);
    fixture.seed_index();
    let baseline = vector_rows(&fixture.index_dir);
    fixture
        .db
        .execute_raw("UPDATE memories SET valid_to = '2099-12-31T00:00:00Z'")
        .expect("bulk metadata update");
    let job = fixture.queue(1, &memory_id(0));
    let expected = fixture.snapshot();
    let streamed = fixture
        .streamed()
        .expect("257 metadata changes remain streamed");
    assert_eq!(streamed.reconciliation.delta.upserts.len(), 257);
    assert_eq!(
        serde_json::to_value(streamed.reconciliation.digests).expect("complete streamed digests"),
        canonical_digests(&expected),
    );
    assert_delta(&fixture.process(&job));
    fixture.assert_published(&expected);
    assert_eq!(
        vector_rows(&fixture.index_dir),
        baseline,
        "streamed metadata preserves all existing vectors"
    );
}

fn regular_files(index_dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![index_dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).expect("active directory") {
            let entry = entry.expect("active entry");
            let kind = entry.file_type().expect("active entry type");
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.insert(
                    entry
                        .path()
                        .strip_prefix(index_dir)
                        .expect("active relative path")
                        .to_path_buf(),
                    std::fs::read(entry.path()).expect("active file bytes"),
                );
            }
        }
    }
    files
}

#[test]
fn cancelled_streamed_stage_preserves_active_files_and_retryable_job() {
    let fixture = Fixture::new(16);
    fixture.seed_index();
    let baseline = regular_files(&fixture.index_dir);
    let generation = read_index_metadata(&fixture.index_dir).generation;
    fixture.db.execute_raw(&format!(
        "UPDATE memories SET content = 'A cancelled update must remain retryable.' WHERE id = '{}'",
        memory_id(0),
    )).expect("cancelled mutation");
    let job = fixture.queue(1, &memory_id(0));
    let job_id = job.id.clone();
    assert!(fixture.streamed().is_some());
    let borrowed = &fixture;
    let cancelled = crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        install_before_index_publish_hook(|cx| {
            cx.set_cancel_reason(asupersync::CancelReason::user(
                "cancel streamed publication",
            ));
        });
        process_one_index_job_with_cx(&cx, &borrowed.db, &job, &borrowed.index_dir).await
    })
    .expect("cancellation runtime");
    assert!(
        matches!(&cancelled, Err(IndexRebuildError::Cancelled(_))),
        "{cancelled:?}"
    );
    assert_eq!(regular_files(&fixture.index_dir), baseline);
    assert_eq!(
        read_index_metadata(&fixture.index_dir).generation,
        generation
    );
    let cancelled_job = fixture
        .db
        .get_search_index_job(&job_id)
        .expect("cancelled lookup")
        .expect("cancelled job");
    assert_eq!(
        cancelled_job.status_enum(),
        Some(SearchIndexJobStatus::Cancelled)
    );
    requeue_cancelled_search_index_jobs(&fixture.db, &fixture.workspace_id)
        .expect("retry cancellation");
    assert_delta(&fixture.process(&cancelled_job));
    fixture.assert_published(&fixture.snapshot());
}

fn insert_session(fixture: &Fixture, workspace: &str) -> String {
    let id = crate::models::SessionId::from_uuid(uuid::Uuid::new_v4()).to_string();
    fixture
        .db
        .insert_session(
            &id,
            &crate::db::CreateSessionInput {
                workspace_id: workspace.to_owned(),
                cass_session_id: format!("stream-source-{id}"),
                source_path: Some("/private/transcript.jsonl".to_owned()),
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 8,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(id.as_bytes())),
                metadata_json: None,
            },
        )
        .expect("source session");
    id
}

fn insert_evidence(
    fixture: &Fixture,
    workspace: &str,
    session: &str,
    ordinal: u32,
    text: &str,
    quarantined: bool,
) -> String {
    let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(u128::from(ordinal)))
        .to_string();
    fixture
        .db
        .insert_evidence_span(
            &id,
            &crate::db::CreateEvidenceSpanInput {
                workspace_id: workspace.to_owned(),
                session_id: session.to_owned(),
                memory_id: None,
                producer_kind: crate::db::EvidenceProducerKind::CassImport,
                cass_span_id: format!("stream-source-span-{ordinal}"),
                span_kind: "message".to_owned(),
                start_line: ordinal,
                end_line: ordinal,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                excerpt: text.to_owned(),
                content_hash: format!("blake3:{}", blake3::hash(text.as_bytes())),
                metadata_json: None,
                inherited_redaction_classes: if quarantined {
                    vec!["history".to_owned()]
                } else {
                    Vec::new()
                },
            },
        )
        .expect("screened source evidence");
    id
}

#[test]
fn streamed_evidence_matches_canonical_order_and_live_admission_scope() {
    let fixture = Fixture::new(16);
    let session = insert_session(&fixture, &fixture.workspace_id);
    let retained = insert_evidence(
        &fixture,
        &fixture.workspace_id,
        &session,
        10,
        "The source shows the earlier repair succeeded.",
        false,
    );
    let revoked = insert_evidence(
        &fixture,
        &fixture.workspace_id,
        &session,
        20,
        "An earlier observation whose search admission will be withdrawn.",
        false,
    );
    let denied = insert_evidence(
        &fixture,
        &fixture.workspace_id,
        &session,
        40,
        "[REDACTED:history] withheld transcript canary",
        true,
    );
    let foreign = fixture.other_workspace();
    let foreign_session = insert_session(&fixture, &foreign);
    let foreign_span = insert_evidence(
        &fixture,
        &foreign,
        &foreign_session,
        50,
        "An observation owned by a different workspace.",
        false,
    );
    fixture.seed_index();
    let baseline = vector_rows(&fixture.index_dir);
    assert!(
        baseline.contains_key(&session)
            && baseline.contains_key(&retained)
            && baseline.contains_key(&revoked)
    );
    assert!(!baseline.contains_key(&denied) && !baseline.contains_key(&foreign_span));

    fixture
        .db
        .execute_raw(&format!(
            "UPDATE evidence_spans SET search_eligibility = 'quarantined' WHERE id = '{revoked}'",
        ))
        .expect("withdraw evidence admission");
    // Physical insertion order is deliberately the opposite of canonical ID order.
    let later = insert_evidence(
        &fixture,
        &fixture.workspace_id,
        &session,
        32,
        "The later source confirms the rollback restored the previous generation.",
        false,
    );
    let earlier = insert_evidence(
        &fixture,
        &fixture.workspace_id,
        &session,
        31,
        "The earlier source confirms publication keeps its captured generation.",
        false,
    );
    let job = fixture.queue(1, &memory_id(0));
    let expected = fixture.snapshot();
    let streamed = fixture.streamed().expect("small evidence delta");
    assert_eq!(streamed.document_counts, expected.document_counts);
    assert_eq!(
        streamed.reconciliation.delta.removals,
        vec![revoked.clone()]
    );
    let actual_order = streamed
        .reconciliation
        .delta
        .upserts
        .iter()
        .map(|doc| doc.id.clone())
        .collect::<Vec<_>>();
    let expected_order = expected
        .documents
        .iter()
        .filter(|doc| doc.id == earlier || doc.id == later)
        .map(|doc| doc.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(actual_order, expected_order);
    assert_eq!(actual_order.len(), 2);
    assert_eq!(
        serde_json::to_value(streamed.reconciliation.digests).expect("evidence stream digests"),
        canonical_digests(&expected)
    );
    assert_delta(&fixture.process(&job));
    fixture.assert_published(&expected);
    let published = vector_rows(&fixture.index_dir);
    for absent in [&revoked, &denied, &foreign_session, &foreign_span] {
        assert!(
            !published.contains_key(absent),
            "excluded source must not publish: {absent}"
        );
    }
    assert!(published.contains_key(&earlier) && published.contains_key(&later));
}
