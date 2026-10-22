//! Exercise the actual decision writer, schema, rollback, and queued work.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::contradiction_detect::{
    ConflictResolutionOutcome, ConflictResolutionPlan, ConflictResolveRequest,
    ContradictionDetectionConfig, PlannedResolutionAction, ResolveVerb, assemble_conflict_surface,
    plan_conflict_resolution,
};
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

type RowState = Vec<(String, Value)>;
type TableState = (&'static str, Vec<RowState>);

fn row_state(row: &sqlmodel_core::Row) -> RowState {
    // Row's Debug includes a randomized name-to-index lookup map. Compare
    // ordered column names and typed SQL values, not that implementation detail.
    row.iter()
        .map(|(column, value)| (column.to_owned(), value.clone()))
        .collect()
}

struct Fixture {
    root: tempfile::TempDir,
    db: DbConnection,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        std::fs::create_dir_all(path.join(".ee")).unwrap();
        std::fs::write(
            path.join(".ee/config.toml"),
            "[memory]\ninclude_global = false\n",
        )
        .unwrap();
        let db = DbConnection::open_file(path.join(".ee/ee.db")).unwrap();
        db.migrate().unwrap();
        db.insert_workspace(
            &stable_workspace_id(&path),
            &CreateWorkspaceInput {
                path: path.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        Self { root, db }
    }

    fn options(&self, supersedes: Option<&str>) -> DecideRecordOptions<'_> {
        // Supersedes is assigned by callers where its shorter borrow is explicit.
        assert!(supersedes.is_none());
        options(self.root.path())
    }

    fn state(&self) -> Vec<TableState> {
        [
            "memories",
            "memory_tags",
            "memory_links",
            "search_index_jobs",
            "audit_log",
            "memory_anchors",
            "memory_anchor_index",
        ]
        .into_iter()
        .map(|table| {
            let rows = self
                .db
                .query(&format!("SELECT * FROM {table} ORDER BY 1, 2"), &[])
                .unwrap();
            (table, rows.iter().map(row_state).collect())
        })
        .collect()
    }

    fn list(&self, history: bool) -> DecideListReport {
        decide_list(&DecideListOptions {
            workspace_path: self.root.path(),
            database_path: None,
            about: None,
            include_superseded: history,
            limit: 100,
            now: None,
        })
        .unwrap()
    }

    fn seed_conflict_memory(&self, kind: &str, level: &str) -> String {
        let id = MemoryId::now().to_string();
        self.db
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: stable_workspace_id(&self.root.path().canonicalize().unwrap()),
                    level: level.to_owned(),
                    kind: kind.to_owned(),
                    content: format!("conflict source {id}"),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .unwrap();
        id
    }

    fn seed_conflict_link(
        &self,
        keeper: &str,
        loser: &str,
        relation: MemoryLinkRelation,
    ) -> String {
        let id = format!(
            "link_{}",
            MemoryId::now().to_string().trim_start_matches("mem_")
        );
        self.db
            .insert_memory_link(
                &id,
                &CreateMemoryLinkInput {
                    src_memory_id: keeper.to_owned(),
                    dst_memory_id: loser.to_owned(),
                    relation,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: true,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: None,
                    metadata_json: None,
                },
            )
            .unwrap();
        id
    }

    fn conflict_plan(&self, keeper: &str, loser: &str) -> ConflictResolutionPlan {
        let surface = assemble_conflict_surface(&self.db, ContradictionDetectionConfig::default());
        let outcome = plan_conflict_resolution(
            &surface,
            &ConflictResolveRequest {
                memory_a: keeper,
                memory_b: loser,
                verb: ResolveVerb::Supersede,
                keep: Some(keeper),
                reason: "A reviewed replacement.",
                scope_a_tags: Vec::new(),
                scope_b_tags: Vec::new(),
            },
        );
        let ConflictResolutionOutcome::Plan(plan) = outcome else {
            panic!("expected conflict plan: {outcome:?}");
        };
        plan
    }

    fn conflict_options<'a>(&'a self, plan: &'a ConflictResolutionPlan) -> DecideRecordOptions<'a> {
        let PlannedResolutionAction::RecordDecision {
            topic,
            chosen,
            alternatives,
            supersedes,
        } = &plan.actions[0]
        else {
            panic!("expected decision atom");
        };
        let mut request = self.options(None);
        request.topic = topic;
        request.chosen = chosen;
        request.alternatives = alternatives.clone();
        request.supersedes = supersedes.as_deref();
        request.rationale = "A reviewed replacement.";
        request
    }
}

fn options(path: &Path) -> DecideRecordOptions<'_> {
    DecideRecordOptions {
        workspace_path: path,
        database_path: None,
        topic: "Storage backend for search",
        chosen: "row store",
        alternatives: vec!["column store".to_owned()],
        rationale: "Keep the implementation deterministic and easy to inspect.",
        revisit_by: None,
        supersedes: None,
        dry_run: false,
        actor: Some("decision-test"),
        now: None,
    }
}

fn injected_error() -> DomainError {
    decide_storage_error("Injected transaction interruption")
}

#[test]
fn row_state_keeps_column_identity_types_order_and_values() {
    let db = DbConnection::open_memory().unwrap();
    let read = |sql: &str| row_state(&db.query(sql, &[]).unwrap()[0]);
    let baseline = read("SELECT 1 AS ordinal, 'one' AS label, NULL AS optional");
    for _ in 0..32 {
        assert_eq!(
            read("SELECT 1 AS ordinal, 'one' AS label, NULL AS optional"),
            baseline
        );
    }
    for sql in [
        "SELECT 2 AS ordinal, 'one' AS label, NULL AS optional",
        "SELECT '1' AS ordinal, 'one' AS label, NULL AS optional",
        "SELECT 1 AS renamed, 'one' AS label, NULL AS optional",
        "SELECT 'one' AS label, 1 AS ordinal, NULL AS optional",
        "SELECT 1 AS ordinal, 'one' AS label, '' AS optional",
    ] {
        assert_ne!(read(sql), baseline, "{sql}");
    }
}

#[test]
fn state_comparison_detects_same_count_source_changes_and_their_rollback() {
    let fixture = Fixture::new();
    let first = record(&fixture.options(None)).unwrap();
    let before = fixture.state();
    for _ in 0..8 {
        assert_eq!(fixture.state(), before);
    }
    let result = fixture.db.with_transaction_error(|| {
        fixture
            .db
            .execute_raw(&format!(
                "UPDATE memories SET content = content || ' changed' WHERE id = '{}'",
                first.decision.memory_id
            ))
            .map_err(RecordError::from)?;
        assert_ne!(fixture.state(), before, "equal row counts are not equality");
        Err::<(), RecordError>(RecordError::from(injected_error()))
    });
    assert!(result.is_err());
    assert_eq!(fixture.state(), before);
}

#[test]
fn replacement_publishes_exact_fields_lineage_audits_and_both_jobs_together() {
    let fixture = Fixture::new();
    let first = record(&fixture.options(None)).unwrap();
    let source = fixture
        .db
        .get_memory(&first.decision.memory_id)
        .unwrap()
        .unwrap();
    let mut request = fixture.options(None);
    request.chosen = "column store, compressed";
    request.alternatives = vec!["row store".to_owned(), "hybrid, indexed".to_owned()];
    request.rationale = "Preserve Unicode café and quoted options exactly.";
    request.supersedes = Some(&first.decision.memory_id);
    let second = record(&request).unwrap();
    assert!(second.persisted);
    assert_eq!(second.decision.chosen, request.chosen);
    assert_eq!(second.decision.alternatives, request.alternatives);
    assert_eq!(second.decision.chain_depth, 1);
    assert_eq!(fixture.list(false).decisions, vec![second.decision.clone()]);
    assert_eq!(fixture.list(true).total_count, 2);
    let predecessor = fixture
        .db
        .get_memory(&first.decision.memory_id)
        .unwrap()
        .unwrap();
    assert_eq!(predecessor.content, source.content);
    assert_eq!(predecessor.level, "episodic");
    assert!(
        fixture
            .db
            .get_memory_superseded_at(&predecessor.id)
            .unwrap()
            .is_some()
    );
    let links = fixture
        .db
        .list_memory_links_for_memory(
            &second.decision.memory_id,
            Some(MemoryLinkRelation::Supersedes),
        )
        .unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].dst_memory_id, predecessor.id);
    assert!(
        second.memory_audit_id.is_some()
            && second.link_audit_id.is_some()
            && second.expire_audit_id.is_some()
    );
    let jobs = fixture
        .db
        .query(
            "SELECT document_id, status FROM search_index_jobs WHERE workspace_id = ?1 ORDER BY id",
            &[Value::Text(second.workspace_id.clone())],
        )
        .unwrap();
    assert_eq!(jobs.len(), 3, "first write, new head, retired predecessor");
    assert!(jobs.iter().all(|row| matches!(
        row.get(1).and_then(Value::as_str),
        Some("pending" | "completed" | "failed")
    )));
    // Best-effort derived publication follows COMMIT. These obligations must
    // survive whether it completes, defers to a coalesced job, or fails.
}

#[test]
fn every_precommit_interruption_rolls_back_the_entire_replacement() {
    let fixture = Fixture::new();
    let first = record(&fixture.options(None)).unwrap();
    let before = fixture.state();
    let audit_path = fixture.root.path().join(".ee/audit.jsonl");
    let audit_before = std::fs::read(&audit_path).unwrap();
    for stop in [
        Stage::Memory,
        Stage::Fields,
        Stage::Link,
        Stage::Predecessor,
        Stage::Index,
        Stage::Report,
    ] {
        let mut request = fixture.options(None);
        request.chosen = "column store";
        request.alternatives = vec!["row store".to_owned()];
        request.supersedes = Some(&first.decision.memory_id);
        let failed = record_with_boundary(&request, |stage, _| {
            if stage == stop {
                Err(injected_error())
            } else {
                Ok(())
            }
        });
        assert!(failed.is_err(), "{stop:?}");
        assert_eq!(fixture.state(), before, "{stop:?}");
        assert_eq!(std::fs::read(&audit_path).unwrap(), audit_before);
    }
    assert_eq!(
        fixture.list(false).decisions[0].memory_id,
        first.decision.memory_id
    );
}

#[test]
fn a_real_late_index_storage_failure_cannot_leave_a_retired_predecessor() {
    let fixture = Fixture::new();
    let first = record(&fixture.options(None)).unwrap();
    let before = fixture.state();
    let mut request = fixture.options(None);
    request.supersedes = Some(&first.decision.memory_id);
    let error = record_with_boundary(&request, |stage, connection| {
        if stage == Stage::Predecessor {
            connection
                .execute_raw("ALTER TABLE search_index_jobs RENAME TO interrupted_jobs")
                .map_err(|_| injected_error())?;
        }
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(error, DomainError::Storage { .. }));
    assert!(!format!("{error:?}").contains("interrupted_jobs"));
    assert_eq!(
        fixture.state(),
        before,
        "DDL, rows, lineage and audits all roll back"
    );
}

#[test]
fn unwinding_inside_the_writer_releases_ownership_and_restores_all_rows() {
    let fixture = Fixture::new();
    let first = record(&fixture.options(None)).unwrap();
    let before = fixture.state();
    let mut request = fixture.options(None);
    request.supersedes = Some(&first.decision.memory_id);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = record_with_boundary(&request, |stage, _| {
            assert_ne!(stage, Stage::Predecessor, "injected unwind");
            Ok(())
        });
    }));
    assert!(result.is_err());
    assert_eq!(fixture.state(), before);
    assert!(
        record(&request).unwrap().persisted,
        "writer ownership was released"
    );
}

#[test]
fn stale_predecessor_replay_is_refused_without_creating_a_fork() {
    let fixture = Fixture::new();
    let first = record(&fixture.options(None)).unwrap();
    let mut request = fixture.options(None);
    request.supersedes = Some(&first.decision.memory_id);
    let second = record(&request).unwrap();
    let before = fixture.state();
    for dry in [false, true] {
        request.dry_run = dry;
        assert!(record(&request).is_err());
        assert_eq!(fixture.state(), before);
    }
    assert_eq!(
        fixture.list(false).decisions[0].memory_id,
        second.decision.memory_id
    );
}

#[test]
fn a_writer_that_wins_after_preparation_is_observed_before_any_source_insert() {
    let fixture = Fixture::new();
    let first = record(&fixture.options(None)).unwrap();
    let mut request = fixture.options(None);
    request.supersedes = Some(&first.decision.memory_id);
    let mut winner = None;
    let error = record_with_boundary(&request, |stage, _| {
        if stage == Stage::Prepared {
            winner = Some(record(&request)?);
        }
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(error, DomainError::NotFound { .. }));
    assert_eq!(
        fixture.list(false).decisions[0].memory_id,
        winner.unwrap().decision.memory_id
    );
    assert_eq!(fixture.list(true).total_count, 2);
}

#[test]
fn concurrent_first_writers_cannot_publish_two_heads_for_one_topic() {
    let fixture = Fixture::new();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let root = fixture.root.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                record(&options(&root))
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(fixture.list(false).total_count, 1);
    assert_eq!(fixture.list(true).total_count, 1);
}

#[test]
fn complete_dry_run_changes_neither_source_rows_nor_audit_stream() {
    let fixture = Fixture::new();
    let first = record(&fixture.options(None)).unwrap();
    let before = fixture.state();
    let db_path = fixture.root.path().join(".ee/ee.db");
    let bytes = std::fs::read(&db_path).unwrap();
    let mut request = fixture.options(None);
    request.dry_run = true;
    request.supersedes = Some(&first.decision.memory_id);
    let report = record(&request).unwrap();
    assert!(!report.persisted && report.dry_run);
    assert_eq!(report.decision.chain_depth, 1);
    assert!(report.memory_index_job_id.is_none() && report.link_audit_id.is_none());
    assert_eq!(fixture.state(), before);
    assert_eq!(std::fs::read(&db_path).unwrap(), bytes);
}

#[test]
fn ancillary_audit_stream_failure_preserves_the_committed_acknowledgement() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root.path().join(".ee/audit.jsonl")).unwrap();
    let report = record(&fixture.options(None)).unwrap();
    assert!(report.persisted);
    assert_eq!(report.status, "recorded");
    assert!(report.warnings[0].contains("Do not repeat"));
    assert!(report.memory_audit_id.is_some() && report.memory_index_job_id.is_some());
    assert_eq!(
        fixture.list(false).decisions[0].memory_id,
        report.decision.memory_id
    );
    assert_eq!(fixture.db.count_table_rows("search_index_jobs").unwrap(), 1);
}

#[test]
fn failed_derived_publication_cannot_turn_the_decision_into_a_failed_source_write() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.root.path().join(".ee/index"),
        b"not an index directory",
    )
    .unwrap();
    let report = record(&fixture.options(None)).unwrap();
    assert!(report.persisted);
    assert_eq!(report.status, "recorded");
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("Do not repeat"))
    );
    assert!(report.memory_audit_id.is_some() && report.memory_index_job_id.is_some());
    assert_eq!(
        fixture.list(false).decisions[0].memory_id,
        report.decision.memory_id
    );
}

#[test]
fn an_earlier_author_expiry_is_not_extended_or_used_as_the_supersession_clock() {
    let fixture = Fixture::new();
    let first = record(&fixture.options(None)).unwrap();
    let earlier = "2020-01-01T00:00:00Z";
    fixture
        .db
        .expire_memory_valid_to(&first.decision.memory_id, earlier)
        .unwrap();
    let mut request = fixture.options(None);
    request.supersedes = Some(&first.decision.memory_id);
    let second = record(&request).unwrap();
    assert_eq!(
        second.superseded.unwrap().valid_to.as_deref(),
        Some(earlier)
    );
    let cutoff = fixture
        .db
        .get_memory_superseded_at(&first.decision.memory_id)
        .unwrap()
        .unwrap();
    assert_ne!(cutoff, earlier);
    assert_eq!(fixture.list(false).total_count, 1);
}

#[test]
fn wrong_topic_and_foreign_predecessors_never_insert_a_partial_decision() {
    let fixture = Fixture::new();
    let first = record(&fixture.options(None)).unwrap();
    let before = fixture.state();
    let mut request = fixture.options(None);
    request.topic = "Transport protocol";
    request.supersedes = Some(&first.decision.memory_id);
    assert!(record(&request).is_err());
    assert_eq!(fixture.state(), before);
    let other = Fixture::new();
    let foreign = record(&other.options(None)).unwrap();
    request.supersedes = Some(&foreign.decision.memory_id);
    assert!(record(&request).is_err());
    assert_eq!(fixture.state(), before);
}

#[test]
fn decoded_structured_secrets_are_refused_before_opening_the_destination() {
    let root = tempfile::tempdir().unwrap();
    for secret in [
        "password=decision-private-canary",
        "safe line\npassword=decision-private-canary",
    ] {
        let mut request = options(root.path());
        request.alternatives.push(secret.to_owned());
        let error = record(&request).unwrap_err();
        assert!(matches!(error, DomainError::PolicyDenied { .. }));
        assert!(!format!("{error:?}").contains("decision-private-canary"));
        assert!(!root.path().join(".ee").exists());
    }
}

#[test]
fn conflict_cli_preview_is_read_only_and_apply_dispatches_the_atomic_writer() {
    let fixture = Fixture::new();
    let keeper = fixture.seed_conflict_memory("fact", "semantic");
    let loser = fixture.seed_conflict_memory("fact", "semantic");
    fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
    let before = fixture.state();
    let invoke = |apply| {
        let mut args = vec![
            "ee",
            "conflict",
            "resolve",
            keeper.as_str(),
            loser.as_str(),
            "--verb",
            "supersede",
            "--keep",
            keeper.as_str(),
            "--reason",
            "The replacement was reviewed.",
            "--workspace",
            fixture.root.path().to_str().unwrap(),
            "--json",
        ];
        if apply {
            args.push("--apply");
        }
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = crate::cli::run(
            args.into_iter().map(std::ffi::OsString::from),
            &mut stdout,
            &mut stderr,
        );
        (
            exit,
            serde_json::from_slice::<JsonValue>(&stdout).unwrap(),
            stderr,
        )
    };
    let (exit, preview, stderr) = invoke(false);
    assert_eq!(exit, crate::models::ProcessExitCode::Success, "{stderr:?}");
    assert_eq!(preview["data"]["status"], "planned");
    assert_eq!(preview["data"]["persisted"], false);
    assert_eq!(fixture.state(), before);
    let (exit, applied, stderr) = invoke(true);
    assert_eq!(exit, crate::models::ProcessExitCode::Success, "{stderr:?}");
    assert_eq!(applied["data"]["status"], "applied");
    assert_eq!(applied["data"]["persisted"], true);
    assert_eq!(
        applied["data"]["results"][0]["auditIds"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(
        fixture
            .db
            .get_memory_superseded_at(&loser)
            .unwrap()
            .is_some()
    );
    assert_eq!(fixture.list(false).total_count, 1);
    let applied_state = fixture.state();
    let (exit, _, _) = invoke(true);
    assert_eq!(exit, crate::models::ProcessExitCode::Usage);
    assert_eq!(fixture.state(), applied_state);
}

#[test]
fn scope_split_and_both_valid_cli_apply_persist_their_decision_rationales() {
    for verb in ["scope-split", "both-valid"] {
        let fixture = Fixture::new();
        let keeper = fixture.seed_conflict_memory("fact", "semantic");
        let loser = fixture.seed_conflict_memory("fact", "semantic");
        fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
        // Reverse the positional pair to exercise the actual scope-tag consumer.
        let mut args = vec![
            "ee",
            "conflict",
            "resolve",
            loser.as_str(),
            keeper.as_str(),
            "--verb",
            verb,
            "--reason",
            "The scopes were reviewed.",
            "--workspace",
            fixture.root.path().to_str().unwrap(),
            "--json",
            "--apply",
        ];
        if verb == "scope-split" {
            args.extend(["--scope-a-tags", "rust", "--scope-b-tags", "python"]);
        }
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = crate::cli::run(
            args.into_iter().map(std::ffi::OsString::from),
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(exit, crate::models::ProcessExitCode::Success, "{stderr:?}");
        let report: JsonValue = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(report["data"]["status"], "applied");
        let decisions = fixture.list(false);
        assert_eq!(decisions.total_count, 1);
        assert_eq!(
            decisions.decisions[0].rationale,
            "The scopes were reviewed."
        );
        if verb == "scope-split" {
            assert_eq!(fixture.db.get_memory_tags(&loser).unwrap(), vec!["rust"]);
            assert_eq!(fixture.db.get_memory_tags(&keeper).unwrap(), vec!["python"]);
        }
        for id in [&keeper, &loser] {
            let source = fixture.db.get_memory(id).unwrap().unwrap();
            assert!(source.valid_to.is_none() && source.tombstoned_at.is_none());
            assert!(fixture.db.get_memory_superseded_at(id).unwrap().is_none());
        }
    }
}

#[test]
fn conflict_supersession_refuses_a_foreign_workspace_side_without_mutation() {
    let fixture = Fixture::new();
    let keeper = fixture.seed_conflict_memory("fact", "semantic");
    let loser = fixture.seed_conflict_memory("fact", "semantic");
    let foreign_workspace = "wsp_00000000000000000000000073";
    fixture
        .db
        .insert_workspace(
            foreign_workspace,
            &CreateWorkspaceInput {
                path: "/tmp/foreign-conflict-workspace".to_owned(),
                name: None,
            },
        )
        .unwrap();
    fixture
        .db
        .execute_raw(&format!(
            "UPDATE memories SET workspace_id = '{foreign_workspace}' WHERE id = '{loser}'"
        ))
        .unwrap();
    fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
    let plan = fixture.conflict_plan(&keeper, &loser);
    let before = fixture.state();
    assert!(matches!(
        record_conflict(&fixture.conflict_options(&plan), &keeper),
        Err(DomainError::PolicyDenied { .. })
    ));
    assert_eq!(fixture.state(), before);
}

#[test]
fn conflict_supersession_atomically_replaces_facts_and_rules_without_tombstoning() {
    for (kind, level) in [("fact", "semantic"), ("rule", "procedural")] {
        let fixture = Fixture::new();
        let keeper = fixture.seed_conflict_memory(kind, level);
        let loser = fixture.seed_conflict_memory(kind, level);
        fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
        let kept_before = fixture.db.get_memory(&keeper).unwrap().unwrap();
        let lost_before = fixture.db.get_memory(&loser).unwrap().unwrap();
        let plan = fixture.conflict_plan(&keeper, &loser);
        let request = fixture.conflict_options(&plan);
        assert_eq!(request.alternatives, vec![format!("keep {loser}")]);
        let report = record_conflict(&request, &keeper).unwrap();

        assert!(report.persisted);
        assert!(report.decision.supersedes.is_none());
        assert_eq!(report.decision.chain_depth, 0);
        assert_eq!(report.superseded.as_ref().unwrap().memory_id, loser);
        assert_eq!(
            fixture.db.get_memory(&keeper).unwrap().unwrap(),
            kept_before
        );
        let retired = fixture.db.get_memory(&loser).unwrap().unwrap();
        assert_eq!(retired.content, lost_before.content);
        assert!(retired.tombstoned_at.is_none());
        assert!(retired.valid_to.is_some());
        assert!(
            fixture
                .db
                .get_memory_superseded_at(&loser)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            retired.level,
            if level == "semantic" {
                "episodic"
            } else {
                level
            }
        );
        let link = fixture
            .db
            .get_memory_link_by_edge(&keeper, &loser, MemoryLinkRelation::Supersedes)
            .unwrap()
            .unwrap();
        assert!(link.directed);
        assert!(
            report.memory_audit_id.is_some()
                && report.link_audit_id.is_some()
                && report.expire_audit_id.is_some()
        );
        let audit = fixture
            .db
            .query(
                "SELECT details FROM audit_log WHERE id = ?1",
                &[Value::Text(report.expire_audit_id.clone().unwrap())],
            )
            .unwrap();
        let details: JsonValue =
            serde_json::from_str(audit[0].get(0).and_then(Value::as_str).unwrap()).unwrap();
        assert_eq!(details["schema"], CONFLICT_SUPERSESSION_AUDIT_SCHEMA);
        assert_eq!(details["fromMemoryId"], keeper);
        assert_eq!(details["toMemoryId"], loser);
        assert_eq!(details["linkId"], link.id);
        assert_eq!(details["resolutionDecisionId"], report.decision.memory_id);
        let jobs = fixture
            .db
            .query(
                "SELECT document_id FROM search_index_jobs ORDER BY document_id",
                &[],
            )
            .unwrap();
        assert_eq!(
            jobs.len(),
            2,
            "decision publication and retired source reindexing"
        );
        assert!(
            jobs.iter()
                .any(|row| row.get(0).and_then(Value::as_str) == Some(loser.as_str()))
        );
        let surface =
            assemble_conflict_surface(&fixture.db, ContradictionDetectionConfig::default());
        assert!(surface.pairs.is_empty() && surface.clusters.is_empty());
        assert_eq!(fixture.list(false).decisions, vec![report.decision.clone()]);
        let before_replay = fixture.state();
        assert!(matches!(
            record_conflict(&request, &keeper),
            Err(DomainError::UsageCodeWithDetails {
                code: "conflict_resolve_stale_surface",
                ..
            })
        ));
        assert_eq!(fixture.state(), before_replay);
    }
}

#[test]
fn conflict_resolution_reuses_an_existing_supersedes_edge() {
    let fixture = Fixture::new();
    let keeper = fixture.seed_conflict_memory("fact", "semantic");
    let loser = fixture.seed_conflict_memory("fact", "semantic");
    let link_id = fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Supersedes);
    let plan = fixture.conflict_plan(&keeper, &loser);
    let report = record_conflict(&fixture.conflict_options(&plan), &keeper).unwrap();
    assert!(report.persisted && report.link_audit_id.is_none());
    assert!(report.expire_audit_id.is_some());
    let links = fixture
        .db
        .list_memory_links_for_memory(&keeper, Some(MemoryLinkRelation::Supersedes))
        .unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].id, link_id);
    assert!(
        links[0].metadata_json.is_none(),
        "existing link provenance is preserved"
    );
}

#[test]
fn conflict_domain_replacement_preserves_a_keeper_decisions_existing_revision_history() {
    let fixture = Fixture::new();
    let original = record(&fixture.options(None)).unwrap();
    let mut revision = fixture.options(None);
    revision.supersedes = Some(&original.decision.memory_id);
    let keeper = record(&revision).unwrap();
    let loser = fixture.seed_conflict_memory("fact", "semantic");
    fixture.seed_conflict_link(
        &keeper.decision.memory_id,
        &loser,
        MemoryLinkRelation::Contradicts,
    );
    let plan = fixture.conflict_plan(&keeper.decision.memory_id, &loser);
    let resolved =
        record_conflict(&fixture.conflict_options(&plan), &keeper.decision.memory_id).unwrap();
    assert_eq!(resolved.decision.chain_depth, 0);
    let current = fixture.list(false);
    assert_eq!(current.total_count, 2);
    let retained = current
        .decisions
        .iter()
        .find(|item| item.memory_id == keeper.decision.memory_id)
        .unwrap();
    assert_eq!(retained.chain_depth, 1);
    assert_eq!(
        retained.supersedes.as_deref(),
        Some(original.decision.memory_id.as_str())
    );
    assert_eq!(fixture.list(true).total_count, 3);
}

#[test]
fn every_precommit_interruption_rolls_back_the_entire_conflict_supersession() {
    let fixture = Fixture::new();
    let keeper = fixture.seed_conflict_memory("fact", "semantic");
    let loser = fixture.seed_conflict_memory("fact", "semantic");
    fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
    let plan = fixture.conflict_plan(&keeper, &loser);
    let mut request = fixture.conflict_options(&plan);
    request.supersedes = None;
    let before = fixture.state();
    for stop in [
        Stage::Memory,
        Stage::Fields,
        Stage::Link,
        Stage::Predecessor,
        Stage::Index,
        Stage::Report,
    ] {
        let failed = record_with_conflict_boundary(
            &request,
            Some(ConflictSupersession {
                keeper: &keeper,
                loser: &loser,
            }),
            |stage, _| {
                if stage == stop {
                    Err(injected_error())
                } else {
                    Ok(())
                }
            },
        );
        assert!(failed.is_err(), "{stop:?}");
        assert_eq!(fixture.state(), before, "{stop:?}");
        assert!(!fixture.root.path().join(".ee/audit.jsonl").exists());
    }
}

#[test]
fn a_conflict_resolved_after_preparation_is_refused_before_source_insert() {
    let fixture = Fixture::new();
    let keeper = fixture.seed_conflict_memory("fact", "semantic");
    let loser = fixture.seed_conflict_memory("fact", "semantic");
    fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
    let plan = fixture.conflict_plan(&keeper, &loser);
    let original = fixture.conflict_options(&plan);
    let mut request = original.clone();
    request.supersedes = None;
    let mut winning_state = None;
    let failed = record_with_conflict_boundary(
        &request,
        Some(ConflictSupersession {
            keeper: &keeper,
            loser: &loser,
        }),
        |stage, _| {
            if stage == Stage::Prepared {
                record_conflict(&original, &keeper)?;
                winning_state = Some(fixture.state());
            }
            Ok(())
        },
    );
    assert!(matches!(
        failed,
        Err(DomainError::UsageCodeWithDetails {
            code: "conflict_resolve_stale_surface",
            ..
        })
    ));
    assert_eq!(fixture.state(), winning_state.unwrap());
    assert_eq!(fixture.list(false).total_count, 1);
}
