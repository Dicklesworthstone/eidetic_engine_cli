//! Source visibility must not turn a hidden decision into a vacant topic.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{CreateFeedbackQuarantineInput, CreateMemoryInput, CreateWorkspaceInput};
use crate::models::{MemoryId, WorkspaceId};

const PRIVATE_TOPIC: &str = "Reserved deployment";
const PRIVATE_CHOICE: &str = "PRIVATE-DECISION-CANARY";
const TIME: &str = "2020-01-01T00:00:00Z";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

struct Fixture {
    db: DbConnection,
    root: tempfile::TempDir,
    workspace: String,
    other: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        std::fs::create_dir_all(path.join(".ee")).unwrap();
        std::fs::write(path.join(".ee/config.toml"), "[memory]\ninclude_global = false\n")
            .unwrap();
        let db = DbConnection::open_file(path.join(".ee/ee.db")).unwrap();
        db.migrate().unwrap();
        let workspace = stable_workspace_id(&path);
        let other = WorkspaceId::from_uuid(uuid::Uuid::from_u128(975)).to_string();
        for (id, path) in [(&workspace, path.clone()), (&other, path.join("other"))] {
            db.insert_workspace(
                id,
                &CreateWorkspaceInput {
                    path: path.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .unwrap();
        }
        Self { db, root, workspace, other }
    }

    fn seed(&self, number: u128, topic: &str) -> String {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: self.workspace.clone(),
                    content: format!("Topic: {topic}\nChosen: {PRIVATE_CHOICE}"),
                    level: "semantic".to_owned(),
                    kind: "decision".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    provenance_uri: Some("manual://decision-admission".to_owned()),
                    tags: vec!["decision".to_owned()],
                    valid_from: Some(TIME.to_owned()),
                    valid_to: None,
                },
            )
            .unwrap();
        self.db
            .set_memory_typed_fields_json(
                &id,
                Some(&json!({
                    "chosen": PRIVATE_CHOICE,
                    "options": [PRIVATE_CHOICE, "alternative"],
                    "rationale": "A deliberate local decision.",
                    "revisit_by": "2027-01-01T00:00:00Z"
                }).to_string()),
            )
            .unwrap();
        id
    }

    fn hold(&self, number: u32, workspace: &str, kind: &str, target: &str) -> String {
        let id = format!("fq_{number:026}");
        self.db
            .insert_feedback_quarantine(
                &id,
                &CreateFeedbackQuarantineInput {
                    workspace_id: workspace.to_owned(),
                    source_id: "PRIVATE-REVIEW-SOURCE".to_owned(),
                    target_type: kind.to_owned(),
                    target_id: target.to_owned(),
                    signal: "harmful".to_owned(),
                    weight: 1.0,
                    source_type: "outcome_observed".to_owned(),
                    proposed_event_id: None,
                    recorded_at: TIME.to_owned(),
                    reason: "PRIVATE-REVIEW-REASON".to_owned(),
                    event_reason: None,
                    evidence_json: None,
                    session_id: None,
                    raw_event_hash: format!("blake3:{}", "a".repeat(64)),
                },
            )
            .unwrap();
        id
    }

    fn seal(&self, id: &str) {
        self.db
            .insert_memory_seal(id, &format!("blake3:{}", "a".repeat(64)), TIME)
            .unwrap();
    }

    fn review(&self, id: &str, status: &str) {
        assert!(self.db.update_feedback_quarantine_status(id, status, Some("operator"), None).unwrap());
    }

    fn scope(&self) -> DecideScope {
        decide_scope(self.root.path(), None, false).unwrap()
    }

    fn list(&self, history: bool, limit: usize) -> DecideListReport {
        decide_list(&DecideListOptions {
            workspace_path: self.root.path(),
            database_path: None,
            about: None,
            include_superseded: history,
            limit,
            now: Some(now()),
        }).unwrap()
    }

    fn request(&self) -> DecideRecordOptions<'_> {
        DecideRecordOptions {
            workspace_path: self.root.path(),
            database_path: None,
            topic: PRIVATE_TOPIC,
            chosen: "reviewed replacement",
            alternatives: vec!["alternative".to_owned()],
            rationale: "Use the explicitly reviewed choice.",
            revisit_by: None,
            supersedes: None,
            dry_run: false,
            actor: Some("decision-admission-test"),
            now: Some(now()),
        }
    }

    fn state(&self) -> Vec<Vec<Vec<(String, Value)>>> {
        [
            "memories", "memory_tags", "memory_links", "search_index_jobs", "audit_log",
            "memory_seals", "feedback_quarantine", "memory_anchors", "memory_anchor_index",
        ].into_iter().map(|table| {
            self.db.query(&format!("SELECT * FROM {table} ORDER BY 1, 2"), &[]).unwrap()
                .into_iter().map(|row| {
                    row.iter().map(|(name, value)| (name.to_owned(), value.clone())).collect()
                }).collect()
        }).collect()
    }
}

#[test]
fn sealed_and_held_decisions_are_absent_from_public_history_and_revisit_counts() {
    for sealed in [false, true] {
        let fixture = Fixture::new();
        let hidden = fixture.seed(980, PRIVATE_TOPIC);
        if sealed { fixture.seal(&hidden); }
        else { fixture.hold(1, &fixture.workspace, "memory", &hidden); }
        let before = fixture.state();
        for history in [false, true] {
            let report = fixture.list(history, 1);
            assert_eq!(report.total_count, 0);
            assert_eq!(report.returned_count, 0);
            assert!(!report.truncated);
            let text = report.data_json().to_string();
            for private in [hidden.as_str(), PRIVATE_CHOICE, PRIVATE_TOPIC, "PRIVATE-REVIEW"] {
                assert!(!text.contains(private));
            }
        }
        let revisit = decide_revisit(&DecideRevisitOptions {
            workspace_path: fixture.root.path(), database_path: None,
            warning_days: Some(14), limit: 1, now: Some(now()),
        }).unwrap();
        assert_eq!(revisit.due_count, 0);
        assert!(revisit.decisions.is_empty());
        assert_eq!(fixture.state(), before);
        assert!(!fixture.root.path().join(".ee/index").exists());
    }
}

#[test]
fn denied_bodies_and_malformed_sidecars_never_reach_the_public_hydrator() {
    let fixture = Fixture::new();
    let sealed = fixture.seed(981, PRIVATE_TOPIC);
    let held = fixture.seed(982, "Another private topic");
    let visible = fixture.seed(983, "Public choice");
    fixture.seal(&sealed);
    fixture.hold(1, &fixture.workspace, "memory", &held);
    for id in [&sealed, &held] {
        fixture.db.execute_raw(&format!(
            "UPDATE memories SET typed_fields_json = '{{\"chosen\":7}}' WHERE id = '{id}'"
        )).unwrap();
    }
    let mut hydrated = Vec::new();
    let result = read_with_observer(&fixture.db, &mut fixture.scope(), true, now(), |ids| {
        hydrated.extend(ids.iter().map(|id| (*id).to_owned()));
        Ok(())
    }).unwrap();
    assert_eq!(hydrated, [visible.clone()]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].memory_id, visible);
}

#[test]
fn review_matches_native_ownership_and_all_holds_must_close_without_unsealing() {
    let fixture = Fixture::new();
    let memory = fixture.seed(984, PRIVATE_TOPIC);
    fixture.hold(1, &fixture.other, "memory", &memory);
    fixture.hold(2, &fixture.workspace, "rule", &memory);
    assert_eq!(fixture.list(false, 0).total_count, 1);
    let first = fixture.hold(3, &fixture.workspace, "memory", &memory);
    let second = fixture.hold(4, &fixture.workspace, "memory", &memory);
    fixture.review(&first, "released");
    assert_eq!(fixture.list(false, 0).total_count, 0);
    fixture.seal(&memory);
    fixture.review(&second, "rejected");
    assert_eq!(fixture.list(false, 0).total_count, 0);
    assert!(fixture.db.mark_memory_seal_revealed(&memory, TIME).unwrap());
    let before = fixture.state();
    assert_eq!(fixture.list(false, 0).decisions[0].memory_id, memory);
    assert_eq!(fixture.state(), before);
}

#[test]
fn held_prefixes_cannot_consume_result_slots_or_stop_keyset_paging() {
    let fixture = Fixture::new();
    fixture.db.with_transaction(|| {
        for number in 1000..1513 {
            let id = fixture.seed(number, PRIVATE_TOPIC);
            fixture.hold(number as u32, &fixture.workspace, "memory", &id);
        }
        Ok(())
    }).unwrap();
    let visible = fixture.seed(1513, "Last visible decision");
    let report = fixture.list(false, 1);
    assert_eq!(report.total_count, 1);
    assert!(!report.truncated);
    assert_eq!(report.decisions[0].memory_id, visible);
}

#[test]
fn concurrent_hold_and_release_are_observed_only_by_the_next_read_snapshot() {
    let fixture = Fixture::new();
    let target = fixture.seed(985, PRIVATE_TOPIC);
    fixture.seed(986, "Public control");
    let reader = DbConnection::open_file_read_only(fixture.root.path().join(".ee/ee.db")).unwrap();
    let mut hold = None;
    let captured = read_with_observer(&reader, &mut fixture.scope(), false, now(), |_| {
        hold = Some(fixture.hold(1, &fixture.workspace, "memory", &target));
        Ok(())
    }).unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(fixture.list(false, 0).total_count, 1);
    let captured = read_with_observer(&reader, &mut fixture.scope(), false, now(), |_| {
        fixture.review(hold.as_deref().unwrap(), "rejected");
        Ok(())
    }).unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(fixture.list(false, 0).total_count, 2);
    reader.begin_read_snapshot().unwrap();
    reader.rollback_read_snapshot().unwrap();
}

#[test]
fn authority_failure_cannot_return_a_partial_result_or_release_a_borrowed_transaction() {
    let fixture = Fixture::new();
    fixture.seed(987, PRIVATE_TOPIC);
    fixture.db.execute_raw("ALTER TABLE feedback_quarantine RENAME TO private_unavailable_review").unwrap();
    let error = load(&mut fixture.scope(), false, now()).unwrap_err();
    assert!(matches!(error, DomainError::Storage { .. }));
    assert!(!format!("{error:?}").contains("private_unavailable_review"));
    fixture.db.begin_read_snapshot().unwrap();
    let request = fixture.request();
    let fields = prepare_decision_fields(request.topic, request.chosen, &request.alternatives,
        request.rationale, None, None, now()).unwrap();
    assert!(record_heads_in_current_snapshot(&fixture.db, &fixture.workspace, &fields, now()).is_err());
    assert!(fixture.db.begin_read_snapshot().is_err(), "caller still owns the transaction");
    fixture.db.rollback_read_snapshot().unwrap();
}

#[test]
fn empty_decision_stores_do_not_require_seal_or_review_tables() {
    let fixture = Fixture::new();
    fixture.db.execute_raw("ALTER TABLE feedback_quarantine RENAME TO unavailable_review").unwrap();
    fixture.db.execute_raw("ALTER TABLE memory_seals RENAME TO unavailable_seals").unwrap();
    assert_eq!(fixture.list(false, 0).total_count, 0);
}

#[test]
fn preview_and_writer_refuse_hidden_predecessors_and_hidden_topic_collisions() {
    for sealed in [false, true] {
        let fixture = Fixture::new();
        let target = fixture.seed(988, PRIVATE_TOPIC);
        if sealed { fixture.seal(&target); }
        else { fixture.hold(1, &fixture.workspace, "memory", &target); }
        let before = fixture.state();
        for dry_run in [false, true] {
            for supersedes in [None, Some(target.as_str())] {
                let mut request = fixture.request();
                request.dry_run = dry_run;
                request.supersedes = supersedes;
                let error = decide_record(&request).unwrap_err();
                assert!(matches!(error, DomainError::PolicyDenied { .. }), "{error:?}");
                for private in [target.as_str(), PRIVATE_TOPIC, PRIVATE_CHOICE, "PRIVATE-REVIEW"] {
                    assert!(!format!("{error:?}").contains(private));
                }
                assert_eq!(fixture.state(), before);
            }
        }
    }
}

#[test]
fn unrelated_review_holds_do_not_block_recording_or_require_private_sidecars() {
    let fixture = Fixture::new();
    let target = fixture.seed(989, PRIVATE_TOPIC);
    fixture.hold(1, &fixture.workspace, "memory", &target);
    fixture.db.execute_raw(&format!(
        "UPDATE memories SET typed_fields_json = '{{\"chosen\":7}}' WHERE id = '{target}'"
    )).unwrap();
    let original = fixture.db.get_memory(&target).unwrap();
    let mut request = fixture.request();
    request.topic = "Independent public deployment";
    request.dry_run = true;
    assert!(!decide_record(&request).unwrap().persisted);
    request.dry_run = false;
    let report = decide_record(&request).unwrap();
    assert!(report.persisted);
    assert_eq!(fixture.db.get_memory(&target).unwrap(), original);
    assert_eq!(fixture.list(false, 0).decisions[0].memory_id, report.decision.memory_id);
}

#[test]
fn a_sealed_current_topic_is_unknown_not_vacant_but_sealed_history_does_not_block() {
    let fixture = Fixture::new();
    let target = fixture.seed(990, PRIVATE_TOPIC);
    fixture.seal(&target);
    fixture.db.execute_raw(&format!(
        "UPDATE memories SET content = '{}' WHERE id = '{target}'",
        crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
    )).unwrap();
    let mut request = fixture.request();
    request.topic = "Independent public deployment";
    for dry in [true, false] {
        request.dry_run = dry;
        assert!(matches!(decide_record(&request), Err(DomainError::PolicyDenied { .. })));
    }
    assert!(fixture.db.restore_imported_memory_supersession(&target, TIME).unwrap());
    assert!(decide_record(&request).unwrap().persisted);
    assert_eq!(fixture.list(true, 0).total_count, 1);
}

#[test]
fn releasing_review_does_not_resurrect_a_superseded_decision_head() {
    let fixture = Fixture::new();
    let target = fixture.seed(991, PRIVATE_TOPIC);
    let hold = fixture.hold(1, &fixture.workspace, "memory", &target);
    fixture.db.restore_imported_memory_supersession(&target, TIME).unwrap();
    fixture.review(&hold, "rejected");
    assert_eq!(fixture.list(false, 0).total_count, 0);
    assert_eq!(fixture.list(true, 0).total_count, 1);
    let mut request = fixture.request();
    request.supersedes = Some(&target);
    for dry in [true, false] {
        request.dry_run = dry;
        assert!(matches!(decide_record(&request), Err(DomainError::NotFound { .. })));
    }
}
