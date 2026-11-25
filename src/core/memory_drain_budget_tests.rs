//! Exercise the production leadership loop with a deterministic monotonic
//! clock, and its real index adapter without waiting for a deadline to elapse.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const OWN_JOB: &str = "sidx_00000000000000000000000501";
const PEER_JOB: &str = "sidx_00000000000000000000000502";

thread_local! {
    static BEFORE_LEASE_RELEASE: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

pub(super) fn before_remember_lease_release() {
    let hook = BEFORE_LEASE_RELEASE.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

struct LeaseReleaseHook;

impl Drop for LeaseReleaseHook {
    fn drop(&mut self) {
        BEFORE_LEASE_RELEASE.with(|slot| *slot.borrow_mut() = None);
    }
}

fn install_lease_release_hook(hook: impl FnOnce() + 'static) -> LeaseReleaseHook {
    BEFORE_LEASE_RELEASE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
    LeaseReleaseHook
}

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    database: PathBuf,
    index: PathBuf,
    db: DbConnection,
    workspace: String,
    memory: String,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary workspace");
        let root = directory.path().canonicalize().expect("workspace path");
        fs::create_dir_all(root.join(".ee")).expect("store directory");
        let database = root.join(".ee/ee.db");
        let db = DbConnection::open_file(&database).expect("source database");
        db.migrate().expect("schema");
        let workspace = stable_workspace_id(&root);
        db.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: root.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .expect("workspace row");
        let fixture = Self {
            _directory: directory,
            index: root.join(".ee/index"),
            root,
            database,
            db,
            workspace,
            memory: crate::testing::mem("remember-drain-deadline"),
        };
        fixture
            .db
            .with_transaction(|| {
                fixture.db.insert_memory(
                    &fixture.memory,
                    &CreateMemoryInput {
                        workspace_id: fixture.workspace.clone(),
                        level: "episodic".to_owned(),
                        kind: "fact".to_owned(),
                        content: "The committed indigo memory survives publication deadlines."
                            .to_owned(),
                        workflow_id: None,
                        confidence: 0.8,
                        utility: 0.0,
                        importance: 0.0,
                        provenance_uri: None,
                        trust_class: "human_explicit".to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )?;
                fixture.db.insert_audit(
                    &crate::testing::audit("remember-drain-deadline"),
                    &CreateAuditInput {
                        workspace_id: Some(fixture.workspace.clone()),
                        actor: Some("ee remember".to_owned()),
                        action: audit_actions::MEMORY_CREATE.to_owned(),
                        target_type: Some("memory".to_owned()),
                        target_id: Some(fixture.memory.clone()),
                        details: None,
                    },
                )?;
                fixture
                    .db
                    .insert_search_index_job(OWN_JOB, &fixture.job_input())
            })
            .expect("atomic source, audit and job commit");
        fixture
    }

    fn job_input(&self) -> CreateSearchIndexJobInput {
        CreateSearchIndexJobInput {
            workspace_id: self.workspace.clone(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(self.memory.clone()),
            documents_total: 1,
        }
    }

    fn assert_source_and_audit_survive(&self) {
        let memory = self.db.get_memory(&self.memory).expect("memory read");
        assert_eq!(memory.expect("committed memory").id, self.memory);
        assert_eq!(self.db.count_table_rows("memories").expect("row count"), 1);
        assert_eq!(
            self.db.count_table_rows("audit_log").expect("audit count"),
            1
        );
        assert!(
            self.db
                .is_lock_held(&remember_index_drain_leader_lock(&self.workspace))
                .expect("leadership state")
                .is_none(),
            "every deadline path releases leadership before returning"
        );
    }
}

#[test]
fn repeated_remember_drain_elections_share_one_budget_without_an_ambient_context() {
    let fixture = Fixture::new();
    let _without_ambient = asupersync::Cx::set_current(None);
    let started = Instant::now();
    let now = Cell::new(started);
    let budget = RememberIndexDrainBudget::new(started, Duration::from_secs(10), None);
    let timeouts = RefCell::new(Vec::new());
    let leases = RefCell::new(Vec::new());
    let probes = Cell::new(0);
    let report = remember_drain_leadership_cycles_with_budget(
        &fixture.db,
        &fixture.workspace,
        OWN_JOB,
        &budget,
        &|| now.get(),
        &mut || Ok(()),
        &mut |remaining| {
            timeouts.borrow_mut().push(remaining);
            let lease = fixture
                .db
                .is_lock_held(&remember_index_drain_leader_lock(&fixture.workspace))?
                .expect("drain holds its election lease");
            let acquired =
                DateTime::parse_from_rfc3339(&lease.acquired_at).expect("lease acquisition time");
            let expires =
                DateTime::parse_from_rfc3339(lease.expires_at.as_deref().expect("bounded lease"))
                    .expect("lease expiry time");
            leases.borrow_mut().push((expires - acquired).num_seconds());
            now.set(now.get() + Duration::from_secs(3));
            // A selected-empty snapshot is not progress or quiescence. Keep
            // racing it until the total deadline, across multiple elections.
            Ok(Vec::new())
        },
        &mut || {
            probes.set(probes.get() + 1);
            Some(true)
        },
    );
    assert_eq!(
        *timeouts.borrow(),
        [10, 7, 4, 1].map(Duration::from_secs),
        "the second round and next election consume the original budget"
    );
    assert_eq!(*leases.borrow(), vec![70, 70, 64, 64]);
    assert_eq!(
        probes.get(),
        4,
        "expired work never probes or re-arms the queue"
    );
    assert_eq!(remember_index_status(&report), "queued");
    assert!(
        report
            .error
            .as_deref()
            .is_some_and(|message| message.contains("total publication budget"))
    );
    assert_eq!(
        fixture
            .db
            .get_search_index_job(OWN_JOB)
            .unwrap()
            .unwrap()
            .status_enum(),
        Some(SearchIndexJobStatus::Pending)
    );
    fixture.assert_source_and_audit_survive();
    assert!(!fixture.index.exists());
}

#[test]
fn deadline_during_pending_probe_stops_before_another_round_and_preserves_completed_work() {
    let fixture = Fixture::new();
    fixture
        .db
        .insert_search_index_job(PEER_JOB, &fixture.job_input())
        .unwrap();
    let started = Instant::now();
    let now = Cell::new(started);
    let budget = RememberIndexDrainBudget::new(started, Duration::from_secs(10), None);
    let drains = Cell::new(0);
    let probes = Cell::new(0);
    let report = remember_drain_leadership_cycles_with_budget(
        &fixture.db,
        &fixture.workspace,
        OWN_JOB,
        &budget,
        &|| now.get(),
        &mut || Ok(()),
        &mut |_| {
            drains.set(drains.get() + 1);
            fixture.db.start_search_index_job(OWN_JOB)?;
            fixture.db.complete_search_index_job(OWN_JOB, 1)?;
            Ok(vec![remember_index_job_report_from_durable_state(
                &fixture.db,
                OWN_JOB,
            )])
        },
        &mut || {
            probes.set(probes.get() + 1);
            now.set(started + Duration::from_secs(10));
            Some(true)
        },
    );
    assert_eq!(drains.get(), 1);
    assert_eq!(probes.get(), 1);
    assert_eq!(
        report.outcome, "completed",
        "completed publication is not rolled back"
    );
    assert!(
        report
            .error
            .as_deref()
            .is_some_and(|message| message.contains("not verified complete"))
    );
    assert_eq!(
        fixture
            .db
            .get_search_index_job(PEER_JOB)
            .unwrap()
            .unwrap()
            .status_enum(),
        Some(SearchIndexJobStatus::Pending)
    );
    assert_eq!(
        authoritative_remember_index_status(
            &fixture.workspace,
            &fixture.root,
            &fixture.database,
            &fixture.index,
            &[OWN_JOB.to_owned()],
            &remember_index_status(&report),
        ),
        "queued",
        "a completed own job must not certify a still-pending peer queue"
    );
    fixture.assert_source_and_audit_survive();
}

#[test]
fn configured_timeout_only_shortens_remember_and_lease_rounding_covers_cleanup() {
    assert_eq!(
        remember_index_drain_timeout(Duration::from_secs(3_600)),
        Duration::from_secs(300)
    );
    assert_eq!(
        remember_index_drain_timeout(Duration::from_secs(7)),
        Duration::from_secs(7)
    );
    assert_eq!(
        remember_index_drain_leader_ttl_secs(Duration::from_millis(1)),
        61
    );
    assert_eq!(
        remember_index_drain_leader_ttl_secs(Duration::from_secs(300)),
        360
    );
}

#[test]
fn expired_index_adapter_does_not_rearm_cancelled_jobs_or_mutate_committed_sources() {
    let fixture = Fixture::new();
    fixture.db.start_search_index_job(OWN_JOB).unwrap();
    fixture.db.cancel_running_search_index_job(OWN_JOB).unwrap();
    let before = fixture.db.get_search_index_job(OWN_JOB).unwrap();
    let error = process_pending_index_jobs_coalesced_before(
        &fixture.db,
        &fixture.workspace,
        &fixture.index,
        None,
        Instant::now(),
        None,
    )
    .expect_err("an expired round must stop before queue recovery");
    assert!(matches!(error, IndexRebuildError::Cancelled(_)));
    assert_eq!(fixture.db.get_search_index_job(OWN_JOB).unwrap(), before);
    fixture.assert_source_and_audit_survive();
    assert!(!fixture.index.exists());
}

#[test]
fn real_remember_drain_honors_parent_cancellation_and_releases_leadership() {
    let fixture = Fixture::new();
    let _embedder = super::super::index::install_test_hash_workspace_embedder(&fixture.workspace);
    let observed = Rc::new(RefCell::new(None));
    let capture = Rc::clone(&observed);
    crate::core::run_cli_with_cx(Duration::from_secs(60), |parent| async move {
        let cancellation = parent.clone();
        super::super::index::install_before_index_publish_hook(move |cx| {
            *capture.borrow_mut() = cx.remaining_budget().deadline;
            cancellation.set_cancel_reason(
                asupersync::CancelReason::shutdown().with_message("remember caller stopped"),
            );
        });
        let released = Rc::new(Cell::new(false));
        let cleanup_observed = Rc::clone(&released);
        let _cleanup = install_lease_release_hook(move || {
            let cx = asupersync::Cx::current().expect("cleanup context");
            assert_eq!(
                cx.cancel_reason().expect("cancelled caller").kind,
                asupersync::CancelKind::Shutdown
            );
            // This is the exact checkpoint used by database contention
            // retries. Inject it at the actual elected lease release so the
            // regression does not depend on a thread winning a timed race.
            crate::db::sleep_retry_delay_or_cancel(crate::db::DbOperation::Execute, Duration::ZERO)
                .expect("lease cleanup masks cancellation during storage retries");
            cleanup_observed.set(true);
        });
        let report = remember_lead_coalesced_index_drain(
            &fixture.db,
            &fixture.workspace,
            OWN_JOB,
            &fixture.index,
        );
        assert_eq!(remember_index_status(&report), "queued");
        assert!(report.error.is_some());
        assert_eq!(
            parent.cancel_reason().expect("caller stays cancelled").kind,
            asupersync::CancelKind::Shutdown
        );
        assert!(
            parent.checkpoint().is_err(),
            "mask ends immediately after ownership cleanup"
        );
        assert!(released.get(), "production leadership released its lease");
        assert!(
            observed
                .borrow()
                .is_some_and(|remaining| remaining <= Duration::from_secs(60))
        );
        // Inspect committed state after the cancelled request, rather than
        // making this verification read inherit its deliberately stopped Cx.
        let _readback = asupersync::Cx::set_current(None);
        assert_eq!(
            fixture
                .db
                .get_search_index_job(OWN_JOB)
                .unwrap()
                .unwrap()
                .status_enum(),
            Some(SearchIndexJobStatus::Cancelled)
        );
        fixture.assert_source_and_audit_survive();
        assert!(
            !fixture.index.exists(),
            "cancelled stage cannot publish a live index"
        );
    })
    .expect("test request runtime");
}

#[test]
fn final_remember_verification_reuses_publication_without_rescanning_source_or_history() {
    let fixture = Fixture::new();
    let _embedder = super::super::index::install_test_hash_workspace_embedder(&fixture.workspace);
    let published = process_index_job_for_connection(&fixture.db, OWN_JOB, &fixture.index)
        .expect("publish the committed memory");
    assert_eq!(remember_index_status(&published), "indexed");
    fixture
        .db
        .insert_search_index_job(PEER_JOB, &fixture.job_input())
        .expect("historical job");
    assert!(fixture.db.start_search_index_job(PEER_JOB).unwrap());
    assert!(fixture.db.complete_search_index_job(PEER_JOB, 1).unwrap());
    fixture
        .db
        .execute(
            "UPDATE search_index_jobs SET error_message = ?1 WHERE id = ?2",
            &[
                sqlmodel_core::Value::Text(
                    "Unused historical publication diagnostic. ".repeat(4096),
                ),
                sqlmodel_core::Value::Text(PEER_JOB.to_owned()),
            ],
        )
        .expect("large completed-job diagnostic");
    let source_before = fixture.db.logical_table_digests().unwrap();
    let metadata_path = fixture.index.join("meta.json");
    let metadata_before = fs::read(&metadata_path).unwrap();
    let (status, queries) = crate::db::capture_query_sql_for_test(|| {
        authoritative_remember_index_status(
            &fixture.workspace,
            &fixture.root,
            &fixture.database,
            &fixture.index,
            &[OWN_JOB.to_owned()],
            "indexed",
        )
    });
    assert_eq!(status, "indexed");
    let job_reads = queries
        .iter()
        .filter(|sql| sql.contains("FROM search_index_jobs"))
        .collect::<Vec<_>>();
    assert_eq!(
        job_reads.len(),
        2,
        "required membership and workspace posture"
    );
    assert!(job_reads.iter().all(|sql| {
        !sql.contains("error_message") && !sql.contains("document_id") && !sql.contains("ORDER BY")
    }));
    for sql in &queries {
        let normalized = sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        for table in [
            "memories",
            "sessions",
            "artifacts",
            "procedural_rules",
            "evidence_spans",
        ] {
            assert!(
                !normalized.contains(&format!("from {table} "))
                    && !normalized.ends_with(&format!("from {table}"))
                    && !normalized.contains(&format!("join {table} ")),
                "a current publication must verify without rehydrating {table}: {sql}"
            );
        }
    }

    #[cfg(feature = "lexical-bm25")]
    {
        let pack_search = crate::core::search::run_pack_search(&SearchOptions {
            workspace_path: fixture.root.clone(),
            database_path: Some(fixture.database.clone()),
            index_dir: Some(fixture.index.clone()),
            query: "indigo memory publication deadlines".to_owned(),
            limit: 5,
            speed: crate::search::SpeedMode::Instant,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: crate::core::search::SearchDedupMode::DocId,
            source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
            strict_source_mode: true,
            memory_scope: crate::models::MemoryScope::Workspace,
            no_global: false,
            strict_scope: false,
        })
        .expect("fresh read-only pack search");
        assert_eq!(
            pack_search
                .report
                .results
                .first()
                .map(|hit| hit.doc_id.as_str()),
            Some(fixture.memory.as_str()),
            "immediate read-only retrieval sees the synchronously published memory"
        );
    }
    assert_eq!(fixture.db.logical_table_digests().unwrap(), source_before);
    assert_eq!(fs::read(metadata_path).unwrap(), metadata_before);
}

#[test]
fn final_remember_verification_keeps_live_job_and_generation_failures_authoritative() {
    let fixture = Fixture::new();
    let _embedder = super::super::index::install_test_hash_workspace_embedder(&fixture.workspace);
    let published = process_index_job_for_connection(&fixture.db, OWN_JOB, &fixture.index)
        .expect("publish source");
    assert_eq!(remember_index_status(&published), "indexed");
    let verify = |required: &[String], provisional| {
        authoritative_remember_index_status(
            &fixture.workspace,
            &fixture.root,
            &fixture.database,
            &fixture.index,
            required,
            provisional,
        )
    };
    let required = [OWN_JOB.to_owned()];
    assert_eq!(verify(&required, "indexed"), "indexed");
    assert_eq!(verify(&required, "failed"), "failed");
    fixture
        .db
        .insert_search_index_job(PEER_JOB, &fixture.job_input())
        .unwrap();
    for (status, expected) in [
        ("pending", "queued"),
        ("running", "queued"),
        ("cancelled", "queued"),
        ("failed", "failed"),
        ("completed", "indexed"),
    ] {
        fixture
            .db
            .execute(
                "UPDATE search_index_jobs SET status = ?1 WHERE id = ?2",
                &[
                    sqlmodel_core::Value::Text(status.to_owned()),
                    sqlmodel_core::Value::Text(PEER_JOB.to_owned()),
                ],
            )
            .unwrap();
        assert_eq!(
            verify(&required, "indexed"),
            expected,
            "each call must observe the fresh durable peer state {status}"
        );
        assert_eq!(
            verify(&["sidx_00000000000000000000000999".to_owned()], "indexed"),
            "queued",
            "missing required membership takes precedence even over failed peers"
        );
    }

    let metadata_path = fixture.index.join("meta.json");
    let metadata = fs::read(&metadata_path).unwrap();
    fs::write(&metadata_path, b"{ malformed publication metadata").unwrap();
    assert_eq!(verify(&required, "indexed"), "queued");
    fs::write(&metadata_path, &metadata).unwrap();
    assert_eq!(verify(&required, "indexed"), "indexed");
    fixture
        .db
        .execute(
            "UPDATE memories SET content = ?1 WHERE id = ?2",
            &[
                sqlmodel_core::Value::Text("A later source commit needs publication.".to_owned()),
                sqlmodel_core::Value::Text(fixture.memory.clone()),
            ],
        )
        .unwrap();
    assert_eq!(
        verify(&required, "indexed"),
        "queued",
        "completed jobs cannot certify metadata from an older source generation"
    );
    fixture
        .db
        .execute_raw("ALTER TABLE search_index_jobs RENAME TO unavailable_index_jobs")
        .unwrap();
    assert_eq!(verify(&required, "indexed"), "queued");
    assert_eq!(fs::read(metadata_path).unwrap(), metadata);
}
