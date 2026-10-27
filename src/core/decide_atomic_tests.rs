//! Exercise the actual decision writer, schema, rollback, and queued work.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

type RowState = Vec<(String, Value)>;
type TableState = (&'static str, Vec<RowState>);

const MERGED_CONTENT: &str =
    "The deployment uses configuration alpha for production and beta for local checks.";

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
            "curation_candidates",
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
        self.resolution_plan(keeper, loser, ResolveVerb::Supersede)
    }

    fn resolution_plan(
        &self,
        keeper: &str,
        loser: &str,
        verb: ResolveVerb,
    ) -> ConflictResolutionPlan {
        let surface = assemble_conflict_surface(&self.db, ContradictionDetectionConfig::default());
        let outcome = plan_conflict_resolution(
            &surface,
            &ConflictResolveRequest {
                memory_a: keeper,
                memory_b: loser,
                verb,
                keep: (verb != ResolveVerb::Merge).then_some(keeper),
                reason: "A reviewed replacement.",
                scope_a_tags: if verb == ResolveVerb::Merge {
                    Vec::new()
                } else {
                    vec!["scope-a".to_owned()]
                },
                scope_b_tags: if verb == ResolveVerb::Merge {
                    Vec::new()
                } else {
                    vec!["scope-b".to_owned()]
                },
                merge_content: (verb == ResolveVerb::Merge).then_some(MERGED_CONTENT),
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
        } = plan
            .actions
            .iter()
            .find(|action| matches!(action, PlannedResolutionAction::RecordDecision { .. }))
            .unwrap()
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

    fn approved_conflict_proposal(
        &self,
        keeper: &str,
        loser: &str,
        verb: ResolveVerb,
    ) -> (crate::db::StoredCurationCandidate, ConflictResolutionPlan) {
        let workspace = stable_workspace_id(&self.root.path().canonicalize().unwrap());
        let proposal = self
            .db
            .with_transaction_error(|| {
                crate::core::conflict_proposal::create_in_transaction(
                    &self.db,
                    &workspace,
                    &ConflictResolveRequest {
                        memory_a: keeper,
                        memory_b: loser,
                        verb,
                        keep: (verb != ResolveVerb::Merge).then_some(keeper),
                        reason: "A reviewed replacement.",
                        scope_a_tags: if verb == ResolveVerb::Merge {
                            Vec::new()
                        } else {
                            vec!["scope-a".to_owned()]
                        },
                        scope_b_tags: if verb == ResolveVerb::Merge {
                            Vec::new()
                        } else {
                            vec!["scope-b".to_owned()]
                        },
                        merge_content: (verb == ResolveVerb::Merge).then_some(MERGED_CONTENT),
                    },
                    Some("decision-test"),
                )
                .map_err(RecordError::from)
            })
            .unwrap();
        // The public validate/accept flows have separate CLI coverage. Here
        // seed their durable result to isolate the writer's transaction boundary.
        self.db
            .execute_raw(&format!(
                "UPDATE curation_candidates SET status = 'approved', review_state = 'accepted' WHERE id = '{}'",
                proposal.candidate_id
            ))
            .unwrap();
        let candidate = self
            .db
            .get_curation_candidate(&workspace, &proposal.candidate_id)
            .unwrap()
            .unwrap();
        (candidate, proposal.plan)
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
        6
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
            let provenance = fixture
                .db
                .get_memory_link_by_edge(
                    &decisions.decisions[0].memory_id,
                    id,
                    MemoryLinkRelation::DerivedFrom,
                )
                .unwrap()
                .unwrap();
            assert!(provenance.directed);
        }
        let surface =
            assemble_conflict_surface(&fixture.db, ContradictionDetectionConfig::default());
        assert!(surface.pairs.is_empty() && surface.clusters.is_empty());
        assert!(
            fixture
                .db
                .get_memory_link_by_edge(&keeper, &loser, MemoryLinkRelation::Contradicts,)
                .unwrap()
                .is_some(),
            "the original conflict evidence remains history"
        );
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
        record_resolution(&fixture.conflict_options(&plan), &plan),
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
        let report = record_resolution(&request, &plan).unwrap().decision;

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
            record_resolution(&request, &plan),
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
    let report = record_resolution(&fixture.conflict_options(&plan), &plan)
        .unwrap()
        .decision;
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
    let resolved = record_resolution(&fixture.conflict_options(&plan), &plan)
        .unwrap()
        .decision;
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
    let request = fixture.conflict_options(&plan);
    let before = fixture.state();
    for stop in [
        Stage::Memory,
        Stage::Fields,
        Stage::Link,
        Stage::Predecessor,
        Stage::Index,
        Stage::ResolutionAction,
        Stage::Provenance,
        Stage::Completion,
        Stage::Report,
    ] {
        let failed = record_resolution_with_boundary(&request, &plan, |stage, _| {
            if stage == stop {
                Err(injected_error())
            } else {
                Ok(())
            }
        });
        assert!(failed.is_err(), "{stop:?}");
        assert_eq!(fixture.state(), before, "{stop:?}");
        assert!(!fixture.root.path().join(".ee/audit.jsonl").exists());
    }
}

#[test]
fn conflict_candidate_completion_and_resolution_roll_back_together() {
    for verb in [
        ResolveVerb::Supersede,
        ResolveVerb::RejectOne,
        ResolveVerb::ScopeSplit,
        ResolveVerb::BothValid,
        ResolveVerb::Merge,
    ] {
        let fixture = Fixture::new();
        let keeper = fixture.seed_conflict_memory("fact", "semantic");
        let loser = fixture.seed_conflict_memory("fact", "semantic");
        fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
        let (candidate, plan) = fixture.approved_conflict_proposal(&keeper, &loser, verb);
        let request = fixture.conflict_options(&plan);
        let before = fixture.state();

        // Completion is immediately before the candidate receipt; Report is
        // after it. Both failures must restore the entire reviewed proposal,
        // decision, source mutations, links, audits and queued index work.
        for stop in [Stage::Completion, Stage::Report] {
            let mut reached = false;
            let failed = record_resolution_candidate_with_boundary(
                &request,
                &plan,
                Some(&candidate),
                |stage, connection| {
                    if stage != stop {
                        return Ok(());
                    }
                    reached = true;
                    let current = connection
                        .get_curation_candidate(&candidate.workspace_id, &candidate.id)
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        current.status,
                        if stop == Stage::Report {
                            "applied"
                        } else {
                            "approved"
                        },
                        "the test must reach the intended side of candidate completion"
                    );
                    Err(injected_error())
                },
            );
            assert!(reached && failed.is_err(), "{verb:?} {stop:?}");
            assert_eq!(fixture.state(), before, "{verb:?} {stop:?}");
            assert!(!fixture.root.path().join(".ee/audit.jsonl").exists());
        }

        let committed = record_candidate_resolution(&request, &plan, &candidate).unwrap();
        let applied = fixture
            .db
            .get_curation_candidate(&candidate.workspace_id, &candidate.id)
            .unwrap()
            .unwrap();
        assert_eq!(applied.status, "applied");
        assert_eq!(applied.review_state, "applied");
        assert!(applied.applied_at.is_some());
        let audit_id = committed.candidate_audit_id.as_deref().unwrap();
        let rows = fixture
            .db
            .query(
                "SELECT action, target_id, details FROM audit_log WHERE id = ?1",
                &[Value::Text(audit_id.to_owned())],
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get(0).and_then(Value::as_str),
            Some(audit_actions::CURATION_CANDIDATE_APPLY)
        );
        assert_eq!(
            rows[0].get(1).and_then(Value::as_str),
            Some(candidate.id.as_str())
        );
        let details: JsonValue =
            serde_json::from_str(rows[0].get(2).and_then(Value::as_str).unwrap()).unwrap();
        assert_eq!(
            details["resolutionDecisionId"],
            committed.decision.decision.memory_id
        );
        let surface =
            assemble_conflict_surface(&fixture.db, ContradictionDetectionConfig::default());
        assert!(surface.pairs.is_empty(), "{verb:?}");
    }
}

#[test]
fn reviewed_merge_preserves_both_histories_and_creates_a_conservative_canonical_summary() {
    let fixture = Fixture::new();
    let source_a = fixture.seed_conflict_memory("fact", "semantic");
    let source_b = fixture.seed_conflict_memory("summary", "episodic");
    fixture.seed_conflict_link(&source_a, &source_b, MemoryLinkRelation::Contradicts);
    for (id, end, confidence) in [
        (&source_a, "2098-01-01T00:00:00Z", "0.3"),
        (&source_b, "2099-01-01T00:00:00Z", "0.8"),
    ] {
        fixture.db.execute_raw(&format!(
            "UPDATE memories SET valid_to = '{end}', confidence = {confidence}, workflow_id = 'deployment', trust_subclass = 'source-attestation' WHERE id = '{id}'"
        )).unwrap();
        fixture
            .db
            .add_memory_tags(id, &["deployment".to_owned(), "project".to_owned()])
            .unwrap();
    }
    let before_a = fixture.db.get_memory(&source_a).unwrap().unwrap();
    let before_b = fixture.db.get_memory(&source_b).unwrap().unwrap();
    let (candidate, plan) =
        fixture.approved_conflict_proposal(&source_a, &source_b, ResolveVerb::Merge);
    let report =
        record_candidate_resolution(&fixture.conflict_options(&plan), &plan, &candidate).unwrap();
    let merged_id = report
        .results
        .iter()
        .find(|result| result.action == "mergeMemories")
        .unwrap()
        .created_memory_id
        .as_deref()
        .unwrap();
    assert_ne!(merged_id, source_a);
    assert_ne!(merged_id, source_b);
    assert_ne!(merged_id, report.decision.decision.memory_id);
    let merged = fixture.db.get_memory(merged_id).unwrap().unwrap();
    assert_eq!(merged.content, MERGED_CONTENT);
    assert_eq!(merged.kind, "summary");
    assert_eq!(merged.level, "semantic");
    assert_eq!(merged.trust_class, "agent_assertion");
    assert_eq!(merged.confidence, 0.3);
    assert_eq!(merged.workflow_id.as_deref(), Some("deployment"));
    assert_eq!(merged.valid_to.as_deref(), Some("2098-01-01T00:00:00Z"));
    assert!(merged.valid_from.is_some());
    assert_ne!(merged.trust_subclass, before_a.trust_subclass);
    assert!(merged.provenance_chain_hash.is_some());
    assert_eq!(
        merged.provenance_uri,
        Some(format!("ee://curation-candidate/{}", candidate.id))
    );
    assert_eq!(
        fixture.db.get_memory_tags(merged_id).unwrap(),
        vec!["deployment", "project"]
    );
    for source in [before_a, before_b] {
        let retired = fixture.db.get_memory(&source.id).unwrap().unwrap();
        assert_eq!(retired.content, source.content);
        assert_eq!(retired.trust_class, source.trust_class);
        assert_eq!(retired.trust_subclass, source.trust_subclass);
        assert!(retired.tombstoned_at.is_none());
        assert!(retired.valid_to.is_some());
        assert!(
            fixture
                .db
                .get_memory_superseded_at(&source.id)
                .unwrap()
                .is_some()
        );
        for relation in [
            MemoryLinkRelation::DerivedFrom,
            MemoryLinkRelation::Supersedes,
        ] {
            assert!(
                fixture
                    .db
                    .get_memory_link_by_edge(merged_id, &source.id, relation)
                    .unwrap()
                    .unwrap()
                    .directed
            );
        }
    }
    assert!(
        fixture
            .db
            .get_memory_link_by_edge(&source_a, &source_b, MemoryLinkRelation::Contradicts)
            .unwrap()
            .is_some()
    );
    let jobs = fixture
        .db
        .list_search_index_jobs(&candidate.workspace_id, None)
        .unwrap();
    assert_eq!(
        jobs.len(),
        4,
        "summary, rationale, and both source index obligations"
    );
    for id in [
        &source_a,
        &source_b,
        &merged.id,
        &report.decision.decision.memory_id,
    ] {
        assert!(
            jobs.iter()
                .any(|job| job.document_id.as_deref() == Some(id.as_str()))
        );
    }
    let applied = fixture
        .db
        .get_curation_candidate(&candidate.workspace_id, &candidate.id)
        .unwrap()
        .unwrap();
    let before_replay = fixture.state();
    assert_eq!(
        crate::core::conflict_proposal::applied_merge_memory_id(&fixture.db, &applied)
            .unwrap()
            .as_deref(),
        Some(merged_id)
    );
    assert_eq!(fixture.state(), before_replay);
    assert_eq!(fixture.list(false).total_count, 1);
    let completed = crate::core::contradiction_detect::gather_explicit_conflict_edges(&fixture.db);
    assert!(completed.read_error.is_none());
    assert!(
        completed
            .resolved_pairs
            .contains(&(plan.memory_a.clone(), plan.memory_b.clone()))
    );
    assert!(
        assemble_conflict_surface(&fixture.db, ContradictionDetectionConfig::default())
            .pairs
            .is_empty()
    );
    let completion = fixture
        .db
        .query(
            "SELECT details FROM audit_log WHERE id = ?1",
            &[Value::Text(report.candidate_audit_id.clone().unwrap())],
        )
        .unwrap();
    let receipt: JsonValue =
        serde_json::from_str(completion[0].get(0).and_then(Value::as_str).unwrap()).unwrap();
    for unrelated in [&source_a, &report.decision.decision.memory_id] {
        let mut corrupt = receipt.clone();
        corrupt["mergedMemoryId"] = json!(unrelated);
        fixture
            .db
            .execute(
                "UPDATE audit_log SET details = ?1 WHERE id = ?2",
                &[
                    Value::Text(corrupt.to_string()),
                    Value::Text(report.candidate_audit_id.clone().unwrap()),
                ],
            )
            .unwrap();
        let before_refusal = fixture.state();
        assert_eq!(
            crate::core::conflict_proposal::applied_merge_memory_id(&fixture.db, &applied)
                .unwrap_err()
                .code(),
            "conflict_merge_completion_invalid"
        );
        assert_eq!(fixture.state(), before_refusal);
    }
}

#[test]
fn merge_refuses_direct_apply_and_rolls_back_every_source_and_output_boundary() {
    let fixture = Fixture::new();
    let source_a = fixture.seed_conflict_memory("fact", "semantic");
    let source_b = fixture.seed_conflict_memory("fact", "semantic");
    fixture.seed_conflict_link(&source_a, &source_b, MemoryLinkRelation::Contradicts);
    let (candidate, plan) =
        fixture.approved_conflict_proposal(&source_a, &source_b, ResolveVerb::Merge);
    let request = fixture.conflict_options(&plan);
    let before = fixture.state();
    assert_eq!(
        record_resolution(&request, &plan).unwrap_err().code(),
        "conflict_merge_requires_review"
    );
    assert_eq!(fixture.state(), before);
    for (stop, occurrence) in [
        (Stage::Memory, 1),
        (Stage::Fields, 1),
        (Stage::MergeMemory, 1),
        (Stage::Link, 1),
        (Stage::Predecessor, 1),
        (Stage::MergeSource, 1),
        (Stage::Link, 2),
        (Stage::Predecessor, 2),
        (Stage::MergeSource, 2),
        (Stage::Index, 3),
        (Stage::ResolutionAction, 2),
        (Stage::Provenance, 2),
        (Stage::Completion, 1),
        (Stage::Report, 1),
    ] {
        let mut seen = 0;
        let failed = record_resolution_candidate_with_boundary(
            &request,
            &plan,
            Some(&candidate),
            |stage, _| {
                if stage == stop {
                    seen += 1;
                    if seen == occurrence {
                        return Err(injected_error());
                    }
                }
                Ok(())
            },
        );
        assert!(failed.is_err(), "{stop:?} #{occurrence}");
        assert_eq!(
            seen, occurrence,
            "the merge failure boundary must be reached"
        );
        assert_eq!(fixture.state(), before, "{stop:?} #{occurrence}");
        assert!(!fixture.root.path().join(".ee/index").exists());
        assert!(!fixture.root.path().join(".ee/audit.jsonl").exists());
    }
}

#[test]
fn merge_revalidates_source_scope_and_the_exact_reviewed_summary_under_the_writer() {
    for change in ["scope", "body", "summary"] {
        let fixture = Fixture::new();
        let source_a = fixture.seed_conflict_memory("fact", "semantic");
        let source_b = fixture.seed_conflict_memory("fact", "semantic");
        fixture.seed_conflict_link(&source_a, &source_b, MemoryLinkRelation::Contradicts);
        let (candidate, mut plan) =
            fixture.approved_conflict_proposal(&source_a, &source_b, ResolveVerb::Merge);
        if change == "summary" {
            let PlannedResolutionAction::MergeMemories { content } = &mut plan.actions[0] else {
                panic!("merge action");
            };
            *content = "A different summary that was never approved.".to_owned();
        }
        let mut expected = fixture.state();
        let failed = record_resolution_candidate_with_boundary(
            &fixture.conflict_options(&plan),
            &plan,
            Some(&candidate),
            |stage, connection| {
                if stage == Stage::Prepared && change != "summary" {
                    let field = if change == "scope" {
                        "workflow_id"
                    } else {
                        "content"
                    };
                    connection
                        .execute_raw(&format!(
                            "UPDATE memories SET {field} = 'changed source' WHERE id = '{source_b}'"
                        ))
                        .map_err(|_| injected_error())?;
                    expected = fixture.state();
                }
                Ok(())
            },
        );
        assert_eq!(
            failed.unwrap_err().code(),
            "conflict_proposal_stale",
            "{change}"
        );
        assert_eq!(fixture.state(), expected, "{change}");
        assert_eq!(fixture.list(false).total_count, 0);
    }
}

#[test]
fn conflict_candidate_review_and_source_races_refuse_before_any_write() {
    for mutation in ["reject", "candidate_content", "source_authority"] {
        let fixture = Fixture::new();
        let keeper = fixture.seed_conflict_memory("fact", "semantic");
        let loser = fixture.seed_conflict_memory("fact", "semantic");
        fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
        let (candidate, plan) =
            fixture.approved_conflict_proposal(&keeper, &loser, ResolveVerb::BothValid);
        let request = fixture.conflict_options(&plan);
        let mut expected = None;
        let failed = record_resolution_candidate_with_boundary(
            &request,
            &plan,
            Some(&candidate),
            |stage, connection| {
                if stage == Stage::Prepared {
                    let sql = match mutation {
                        "reject" => format!(
                            "UPDATE curation_candidates SET status = 'rejected', review_state = 'rejected' WHERE id = '{}'",
                            candidate.id
                        ),
                        "candidate_content" => format!(
                            "UPDATE curation_candidates SET proposed_content = '{{}}' WHERE id = '{}'",
                            candidate.id
                        ),
                        _ => format!(
                            "UPDATE memories SET trust_class = 'agent_assertion' WHERE id = '{keeper}'"
                        ),
                    };
                    connection.execute_raw(&sql).map_err(|_| injected_error())?;
                    expected = Some(fixture.state());
                }
                Ok(())
            },
        );
        assert!(
            matches!(
                failed,
                Err(DomainError::UsageCodeWithDetails {
                    code: "conflict_proposal_stale",
                    ..
                })
            ),
            "{mutation}: {failed:?}"
        );
        assert_eq!(fixture.state(), expected.unwrap(), "{mutation}");
        assert_eq!(fixture.list(false).total_count, 0);
    }
}

#[test]
fn conflict_candidate_apply_cannot_replace_the_reviewed_rationale() {
    let fixture = Fixture::new();
    let keeper = fixture.seed_conflict_memory("fact", "semantic");
    let loser = fixture.seed_conflict_memory("fact", "semantic");
    fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
    let (candidate, plan) =
        fixture.approved_conflict_proposal(&keeper, &loser, ResolveVerb::BothValid);
    let mut request = fixture.conflict_options(&plan);
    request.rationale = "A different rationale that was never reviewed.";
    let before = fixture.state();
    let result = record_candidate_resolution(&request, &plan, &candidate);
    assert!(matches!(
        result,
        Err(DomainError::UsageCodeWithDetails {
            code: "conflict_proposal_stale",
            ..
        })
    ));
    assert_eq!(fixture.state(), before);
    assert!(!fixture.root.path().join(".ee/audit.jsonl").exists());
}

#[test]
fn a_conflict_resolved_after_preparation_is_refused_before_source_insert() {
    let fixture = Fixture::new();
    let keeper = fixture.seed_conflict_memory("fact", "semantic");
    let loser = fixture.seed_conflict_memory("fact", "semantic");
    fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
    let plan = fixture.conflict_plan(&keeper, &loser);
    let original = fixture.conflict_options(&plan);
    let request = original.clone();
    let mut winning_state = None;
    let failed = record_resolution_with_boundary(&request, &plan, |stage, _| {
        if stage == Stage::Prepared {
            record_resolution(&original, &plan)?;
            winning_state = Some(fixture.state());
        }
        Ok(())
    });
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

#[test]
fn every_non_superseding_resolution_rolls_back_each_action_and_final_receipt() {
    for verb in [
        ResolveVerb::RejectOne,
        ResolveVerb::ScopeSplit,
        ResolveVerb::BothValid,
    ] {
        let fixture = Fixture::new();
        let keeper = fixture.seed_conflict_memory("fact", "semantic");
        let loser = fixture.seed_conflict_memory("fact", "semantic");
        fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
        let plan = fixture.resolution_plan(&keeper, &loser, verb);
        let request = fixture.conflict_options(&plan);
        let before = fixture.state();
        let stops = [
            (Stage::Memory, 1),
            (Stage::Fields, 1),
            (Stage::Provenance, 1),
            (Stage::Provenance, 2),
            (Stage::Completion, 1),
            (Stage::Report, 1),
        ]
        .into_iter()
        .chain((1..=plan.actions.len()).map(|number| (Stage::ResolutionAction, number)));
        for (stop, occurrence) in stops {
            let mut seen = 0;
            let result = record_resolution_with_boundary(&request, &plan, |stage, _| {
                if stage == stop {
                    seen += 1;
                    if seen == occurrence {
                        return Err(injected_error());
                    }
                }
                Ok(())
            });
            assert!(result.is_err(), "{verb:?}: {stop:?} #{occurrence}");
            assert_eq!(
                seen, occurrence,
                "the failure boundary must actually be reached"
            );
            assert_eq!(fixture.state(), before, "{verb:?}: {stop:?} #{occurrence}");
            assert!(!fixture.root.path().join(".ee/index").exists());
            assert!(!fixture.root.path().join(".ee/audit.jsonl").exists());
        }
    }
}

#[test]
fn rejecting_a_memory_retains_its_body_and_records_canonical_lifecycle_and_provenance() {
    let fixture = Fixture::new();
    let keeper = fixture.seed_conflict_memory("fact", "semantic");
    let loser = fixture.seed_conflict_memory("fact", "semantic");
    let original_link =
        fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
    let kept = fixture.db.get_memory(&keeper).unwrap().unwrap();
    let lost = fixture.db.get_memory(&loser).unwrap().unwrap();
    let mut plan = fixture.resolution_plan(&keeper, &loser, ResolveVerb::RejectOne);
    let rationale = "The source says \"withdrawn\".\nThe replacement was reviewed.";
    let PlannedResolutionAction::TombstoneMemory { reason, .. } = &mut plan.actions[0] else {
        panic!("reject must preview its actual tombstone effect");
    };
    *reason = rationale.to_owned();
    let mut request = fixture.conflict_options(&plan);
    request.rationale = rationale;
    let report = record_resolution(&request, &plan).unwrap();
    assert_eq!(report.results[0].action, "tombstoneMemory");
    assert_eq!(fixture.db.get_memory(&keeper).unwrap().unwrap(), kept);
    let retired = fixture.db.get_memory(&loser).unwrap().unwrap();
    assert_eq!(retired.content, lost.content);
    assert!(retired.tombstoned_at.is_some());
    assert!(
        fixture
            .db
            .get_memory_superseded_at(&loser)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .db
            .get_memory_link(&original_link)
            .unwrap()
            .is_some()
    );
    let audits = fixture
        .db
        .query(
            "SELECT action, details FROM audit_log WHERE target_id = ?1 ORDER BY id",
            &[Value::Text(loser.clone())],
        )
        .unwrap();
    let mut has_lifecycle = false;
    let mut has_tombstone = false;
    for audit in audits {
        let action = audit.get(0).and_then(Value::as_str).unwrap();
        let details: JsonValue =
            serde_json::from_str(audit.get(1).and_then(Value::as_str).unwrap()).unwrap();
        if action == audit_actions::MEMORY_TOMBSTONE {
            has_tombstone = true;
            assert_eq!(details["reason"], rationale);
        }
        if action == audit_actions::MEMORY_LEVEL_TRANSITION {
            has_lifecycle = true;
            assert_eq!(details["newLevel"], "tombstoned");
        }
    }
    assert!(has_tombstone && has_lifecycle);
    let decision = &report.decision.decision.memory_id;
    for source in [&keeper, &loser] {
        assert!(
            fixture
                .db
                .get_memory_link_by_edge(decision, source, MemoryLinkRelation::DerivedFrom)
                .unwrap()
                .unwrap()
                .directed
        );
    }
    assert_eq!(fixture.db.count_table_rows("search_index_jobs").unwrap(), 2);
    assert_eq!(
        fixture.list(false).decisions,
        vec![report.decision.decision.clone()]
    );
    assert!(
        assemble_conflict_surface(&fixture.db, ContradictionDetectionConfig::default())
            .pairs
            .is_empty()
    );
    let after = fixture.state();
    assert!(matches!(
        record_resolution(&request, &plan),
        Err(DomainError::UsageCodeWithDetails {
            code: "conflict_resolve_stale_surface",
            ..
        })
    ));
    assert_eq!(fixture.state(), after);
}

#[test]
fn full_plan_revalidation_rejects_extra_or_rebound_mutations_before_writing() {
    let fixture = Fixture::new();
    let keeper = fixture.seed_conflict_memory("fact", "semantic");
    let loser = fixture.seed_conflict_memory("fact", "semantic");
    let unrelated = fixture.seed_conflict_memory("fact", "semantic");
    fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
    let original = fixture.resolution_plan(&keeper, &loser, ResolveVerb::ScopeSplit);
    let before = fixture.state();
    for mutation in 0..4 {
        let mut plan = original.clone();
        match mutation {
            0 => plan.actions.push(PlannedResolutionAction::AddTags {
                memory_id: unrelated.clone(),
                tags: vec!["unreviewed".to_owned()],
            }),
            1 => {
                let PlannedResolutionAction::AddTags { memory_id, .. } = &mut plan.actions[0]
                else {
                    unreachable!()
                };
                *memory_id = unrelated.clone();
            }
            2 => {
                let PlannedResolutionAction::AddTags { tags, .. } = &mut plan.actions[0] else {
                    unreachable!()
                };
                tags.push("unreviewed".to_owned());
            }
            _ => {
                plan.actions.remove(0);
            }
        }
        assert!(matches!(
            record_resolution(&fixture.conflict_options(&plan), &plan),
            Err(DomainError::Usage { .. })
        ));
        assert_eq!(fixture.state(), before);
    }
}

#[test]
fn a_loser_becoming_a_protected_rule_after_planning_refuses_rejection_atomically() {
    let fixture = Fixture::new();
    let keeper = fixture.seed_conflict_memory("fact", "semantic");
    let loser = fixture.seed_conflict_memory("fact", "semantic");
    fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
    let plan = fixture.resolution_plan(&keeper, &loser, ResolveVerb::RejectOne);
    let mut protected_state = None;
    let result =
        record_resolution_with_boundary(&fixture.conflict_options(&plan), &plan, |stage, _| {
            if stage == Stage::Prepared {
                fixture.db.execute_raw(&format!(
                "UPDATE memories SET kind = 'rule', level = 'procedural' WHERE id = '{loser}'"
            )).unwrap();
                protected_state = Some(fixture.state());
            }
            Ok(())
        });
    assert!(matches!(result, Err(DomainError::PolicyDenied { .. })));
    assert_eq!(fixture.state(), protected_state.unwrap());
    assert!(!fixture.root.path().join(".ee/index").exists());
}

#[test]
fn resolution_revises_an_existing_rationale_without_confusing_source_ancestry() {
    for verb in [
        ResolveVerb::Supersede,
        ResolveVerb::RejectOne,
        ResolveVerb::ScopeSplit,
        ResolveVerb::BothValid,
    ] {
        let fixture = Fixture::new();
        let keeper = fixture.seed_conflict_memory("fact", "semantic");
        let loser = fixture.seed_conflict_memory("fact", "semantic");
        fixture.seed_conflict_link(&keeper, &loser, MemoryLinkRelation::Contradicts);
        let plan = fixture.resolution_plan(&keeper, &loser, verb);
        let mut legacy = fixture.conflict_options(&plan);
        legacy.supersedes = None;
        let previous = record(&legacy).unwrap();
        let previous_body = fixture
            .db
            .get_memory(&previous.decision.memory_id)
            .unwrap()
            .unwrap()
            .content;
        let applied = record_resolution(&fixture.conflict_options(&plan), &plan).unwrap();
        let decision = &applied.decision.decision;
        assert_eq!(
            decision.supersedes.as_deref(),
            Some(previous.decision.memory_id.as_str())
        );
        assert_eq!(decision.chain_depth, 1);
        assert_eq!(fixture.list(false).decisions, vec![decision.clone()]);
        assert_eq!(fixture.list(true).total_count, 2);
        assert_eq!(
            fixture
                .db
                .get_memory(&previous.decision.memory_id)
                .unwrap()
                .unwrap()
                .content,
            previous_body
        );
        assert!(
            fixture
                .db
                .get_memory_superseded_at(&previous.decision.memory_id)
                .unwrap()
                .is_some()
        );
        assert!(
            assemble_conflict_surface(&fixture.db, ContradictionDetectionConfig::default())
                .pairs
                .is_empty()
        );
        let receipt = applied
            .results
            .iter()
            .find(|result| result.action == "recordDecision")
            .unwrap();
        assert!(
            receipt.audit_ids.len() >= 6,
            "ordinary revision, provenance and completion remain auditable"
        );
    }
}
