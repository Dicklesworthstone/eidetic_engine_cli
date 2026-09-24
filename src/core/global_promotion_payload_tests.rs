//! Real-store tests for the global source-to-destination payload boundary.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::super::{PromoteGlobalOptions, admission, persist_global_promotion, promote_global};
use super::*;
use crate::core::global_store::{GlobalStorePaths, open_or_create_global_store};
use crate::db::{CreateMemoryInput, CreateWorkspaceInput, MemoryAttemptFamily};
use std::path::PathBuf;

const WORKSPACE: &str = "wsp_00000000000000000000000181";
const OTHER_WORKSPACE: &str = "wsp_00000000000000000000000182";
const MEMORY: &str = "mem_00000000000000000000000181";
const SIBLING: &str = "mem_00000000000000000000000182";
const FAMILY: &str = "private-promotion-attempt-family";
const BODY: &str = "Use the agreed deployment procedure.";

struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    global: GlobalStorePaths,
    db: DbConnection,
}

impl Fixture {
    fn new(kind: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let physical = root.path().canonicalize().unwrap();
        let path = physical.join("source.db");
        let db = DbConnection::open_file(&path).unwrap();
        db.migrate().unwrap();
        for (workspace, name) in [(WORKSPACE, "source"), (OTHER_WORKSPACE, "other")] {
            db.insert_workspace(
                workspace,
                &CreateWorkspaceInput {
                    path: physical.join(name).display().to_string(),
                    name: None,
                },
            )
            .unwrap();
        }
        db.insert_memory(MEMORY, &input(WORKSPACE, kind)).unwrap();
        Self {
            path,
            global: GlobalStorePaths::from_root(&physical.join("global")),
            db,
            _root: root,
        }
    }

    fn fields(&self, value: Value) {
        assert!(
            self.db
                .set_memory_typed_fields_json(MEMORY, Some(&value.to_string()))
                .unwrap()
        );
    }

    fn options(&self, dry_run: bool) -> PromoteGlobalOptions<'_> {
        PromoteGlobalOptions {
            workspace_database_path: &self.path,
            memory_id: MEMORY,
            global_paths: &self.global,
            global_lane_available: true,
            actor: None,
            dry_run,
        }
    }

    fn capture(&self) -> (StoredMemory, PromotionPayload) {
        let (memory, payload, plan) =
            admission::load_source(&self.options(true), chrono::Utc::now()).unwrap();
        assert!(plan.allowed(), "{}", plan.data_json());
        (memory, payload)
    }

    fn destination(&self) -> (DbConnection, String) {
        open_or_create_global_store(&self.global).unwrap()
    }

    fn publish(&self, db: &DbConnection, workspace: &str) -> (String, bool, Option<String>) {
        let (memory, payload) = self.capture();
        persist_global_promotion(db, workspace, &memory, &payload, None, chrono::Utc::now())
            .unwrap()
    }

    fn attach(
        &self,
        memory: &str,
        size: Option<u32>,
        slot: Option<u32>,
        disposition: Option<&str>,
    ) {
        assert!(
            self.db
                .set_memory_attempt_family(
                    memory,
                    &MemoryAttemptFamily {
                        family_id: FAMILY.to_owned(),
                        declared_size: size,
                        attempt_index: slot,
                        disposition: disposition.map(str::to_owned),
                    }
                )
                .unwrap()
        );
    }

    fn complete_family(&self) {
        self.attach(MEMORY, Some(2), Some(1), Some("selected"));
        self.db
            .insert_memory(SIBLING, &input(WORKSPACE, "decision"))
            .unwrap();
        self.attach(SIBLING, Some(2), Some(2), Some("rejected"));
    }
}

fn input(workspace: &str, kind: &str) -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: workspace.to_owned(),
        level: "semantic".to_owned(),
        kind: kind.to_owned(),
        content: BODY.to_owned(),
        workflow_id: None,
        confidence: 0.9,
        utility: 0.5,
        importance: 0.5,
        provenance_uri: Some("manual://promotion-payload".to_owned()),
        trust_class: "human_explicit".to_owned(),
        trust_subclass: None,
        tags: Vec::new(),
        valid_from: Some("2020-01-01T00:00:00Z".to_owned()),
        valid_to: None,
    }
}

fn durable_counts(db: &DbConnection) -> Vec<i64> {
    ["memories", "memory_tags", "search_index_jobs", "audit_log"]
        .into_iter()
        .map(|table| db.count_table_rows(table).unwrap())
        .collect()
}

#[test]
fn structured_decisions_rules_and_commands_survive_publication_and_retry() {
    for (kind, fields) in [
        (
            "decision",
            json!({"chosen":"SQLite", "options":["SQLite","Postgres"], "rationale":"Works offline", "revisit_by":"2027-01-01T00:00:00.123456789Z"}),
        ),
        (
            "rule",
            json!({"condition":"release", "action":"verify rollback", "exceptions":["documentation"]}),
        ),
        (
            "command",
            json!({"command":"cargo fmt --check", "when_to_use":"before release", "exit_meaning":"zero means formatted"}),
        ),
    ] {
        let f = Fixture::new(kind);
        f.fields(fields);
        let original = f.db.get_memory(MEMORY).unwrap();
        let source_fields = f.db.get_memory_typed_fields_json(MEMORY).unwrap();
        let (db, workspace) = f.destination();
        let (id, duplicate, job) = f.publish(&db, &workspace);
        assert!(!duplicate && job.is_some());
        assert_eq!(db.get_memory_typed_fields_json(&id).unwrap(), source_fields);
        let (again, duplicate, again_job) = f.publish(&db, &workspace);
        assert_eq!(again, id);
        assert!(duplicate);
        assert_eq!(job, again_job);
        assert_eq!(db.count_table_rows("memories").unwrap(), 1);
        assert_eq!(f.db.get_memory(MEMORY).unwrap(), original);
        assert_eq!(
            f.db.get_memory_typed_fields_json(MEMORY).unwrap(),
            source_fields
        );
    }
}

#[test]
fn equal_prose_with_different_structured_decisions_never_merges() {
    let f = Fixture::new("decision");
    f.fields(json!({"chosen":"SQLite"}));
    let (db, workspace) = f.destination();
    let (first, _, _) = f.publish(&db, &workspace);
    f.fields(json!({"chosen":"Postgres"}));
    let preview = promote_global(&f.options(true)).unwrap();
    assert!(preview.plan.allowed() && !preview.executed && !preview.already_promoted);
    assert!(preview.global_memory_id.is_none());
    let (second, duplicate, _) = f.publish(&db, &workspace);
    assert!(!duplicate);
    assert_ne!(first, second);
    assert_eq!(db.count_table_rows("memories").unwrap(), 2);
    assert!(
        db.get_memory_typed_fields_json(&first)
            .unwrap()
            .unwrap()
            .contains("SQLite")
    );
    assert!(
        db.get_memory_typed_fields_json(&second)
            .unwrap()
            .unwrap()
            .contains("Postgres")
    );
}

#[test]
fn typed_and_untyped_rows_do_not_alias_in_either_direction() {
    for typed_first in [false, true] {
        let f = Fixture::new("decision");
        if typed_first {
            f.fields(json!({"chosen":"SQLite"}));
        }
        let (db, workspace) = f.destination();
        let (first, _, _) = f.publish(&db, &workspace);
        if typed_first {
            assert!(f.db.set_memory_typed_fields_json(MEMORY, None).unwrap());
        } else {
            f.fields(json!({"chosen":"SQLite"}));
        }
        let (second, duplicate, _) = f.publish(&db, &workspace);
        assert!(!duplicate);
        assert_ne!(first, second);
    }
}

#[test]
fn canonical_equivalent_sidecars_remain_idempotent() {
    let f = Fixture::new("decision");
    f.fields(json!({"chosen":"SQLite", "rationale":"Offline"}));
    let (db, workspace) = f.destination();
    let (id, _, _) = f.publish(&db, &workspace);
    db.execute_raw(&format!(
        "UPDATE memories SET typed_fields_json = '{{\"rationale\":\"Offline\",\"chosen\":\"SQLite\"}}' WHERE id = '{id}'"
    )).unwrap();
    let (again, duplicate, _) = f.publish(&db, &workspace);
    assert!(duplicate);
    assert_eq!(again, id);
}

#[test]
fn sidecar_only_secrets_are_refused_before_any_destination_access() {
    let f = Fixture::new("decision");
    let secret = "AKIAIOSFODNN7EXAMPLE";
    f.fields(json!({"chosen":"SQLite", "rationale":format!("Deploy key: {secret}")}));
    std::fs::write(&f.global.root, b"unusable destination").unwrap();
    let before = durable_counts(&f.db);
    for dry_run in [false, true] {
        let report = promote_global(&f.options(dry_run)).unwrap();
        assert!(!report.executed && !report.plan.allowed());
        let output = report.data_json();
        assert_eq!(
            output["plan"]["detail"]["code"],
            "global_promotion_redaction_refused"
        );
        assert!(!output.to_string().contains(secret));
        assert_eq!(
            std::fs::read(&f.global.root).unwrap(),
            b"unusable destination"
        );
        assert_eq!(durable_counts(&f.db), before);
    }
}

#[test]
fn malformed_sidecar_is_not_silently_dropped_or_echoed() {
    let f = Fixture::new("decision");
    f.db.execute_raw(&format!(
        "UPDATE memories SET typed_fields_json = '{{\"private-invalid-field\":\"private-value\"}}' WHERE id = '{MEMORY}'"
    )).unwrap();
    for dry_run in [false, true] {
        let error = promote_global(&f.options(dry_run)).unwrap_err();
        assert!(error.contains("typed fields"));
        assert!(!error.contains("private"));
        assert!(!f.global.root.exists());
    }
}

#[test]
fn incomplete_or_unslotted_attempts_cannot_use_an_old_high_trust_label() {
    for (size, slot, disposition) in [
        (Some(18), Some(1), Some("selected")),
        (Some(2), None, None),
        (None, Some(1), Some("selected")),
    ] {
        for trust in ["human_explicit", "agent_validated"] {
            let f = Fixture::new("decision");
            f.attach(MEMORY, size, slot, disposition);
            f.db.execute_raw(&format!(
                "UPDATE memories SET trust_class = '{trust}' WHERE id = '{MEMORY}'"
            ))
            .unwrap();
            std::fs::write(&f.global.root, b"unusable destination").unwrap();
            let before = durable_counts(&f.db);
            for dry_run in [false, true] {
                let report = promote_global(&f.options(dry_run)).unwrap();
                assert!(!report.executed && !report.plan.allowed());
                let output = report.data_json();
                assert_eq!(
                    output["plan"]["detail"]["code"],
                    "global_promotion_evidence_gate"
                );
                assert!(
                    output["plan"]["detail"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("Attempt-family")
                );
                assert!(!output.to_string().contains(FAMILY));
                assert_eq!(durable_counts(&f.db), before);
                assert_eq!(
                    std::fs::read(&f.global.root).unwrap(),
                    b"unusable destination"
                );
            }
        }
    }
}

#[test]
fn family_members_in_another_workspace_do_not_complete_the_source_family() {
    let f = Fixture::new("decision");
    f.attach(MEMORY, Some(2), Some(1), Some("selected"));
    f.db.insert_memory(SIBLING, &input(OTHER_WORKSPACE, "decision"))
        .unwrap();
    f.attach(SIBLING, Some(2), Some(2), Some("rejected"));
    let report = promote_global(&f.options(false)).unwrap();
    assert!(!report.plan.allowed());
    assert!(!f.global.root.exists());
}

#[test]
fn revision_stable_ledger_membership_survives_a_cleared_pointer() {
    let f = Fixture::new("decision");
    f.attach(MEMORY, Some(2), Some(1), Some("selected"));
    f.db.execute_raw(&format!("UPDATE memories SET attempt_family_id = NULL, attempt_family_size = NULL WHERE id = '{MEMORY}'")).unwrap();
    let report = promote_global(&f.options(false)).unwrap();
    assert!(!report.plan.allowed());
    assert!(!f.global.root.exists());
}

#[test]
fn tombstoning_a_negative_sibling_revokes_family_eligibility() {
    let f = Fixture::new("decision");
    f.complete_family();
    assert!(promote_global(&f.options(true)).unwrap().plan.allowed());
    f.db.execute_raw(&format!(
        "UPDATE memories SET tombstoned_at = '2025-01-01T00:00:00Z' WHERE id = '{SIBLING}'"
    ))
    .unwrap();
    assert!(!promote_global(&f.options(false)).unwrap().plan.allowed());
    assert!(!f.global.root.exists());
}

#[test]
fn complete_family_preserves_sanitized_evidence_and_typed_commitment_in_audit() {
    let f = Fixture::new("decision");
    f.complete_family();
    f.fields(json!({"chosen":"PRIVATE-PAYLOAD-MARKER"}));
    let (db, workspace) = f.destination();
    let (id, _, _) = f.publish(&db, &workspace);
    let raw = db.get_memory_typed_fields_json(&id).unwrap().unwrap();
    let rows = db
        .query(
            "SELECT details FROM audit_log WHERE action = 'memory.promote_global'",
            &[],
        )
        .unwrap();
    let text = rows[0].get(0).unwrap().as_str().unwrap();
    assert!(!text.contains(FAMILY));
    assert!(!text.contains("PRIVATE-PAYLOAD-MARKER"));
    let data: Value = serde_json::from_str(text).unwrap();
    let evidence = &data["sourcePayload"];
    assert_eq!(
        evidence["typedFieldsHash"],
        format!("blake3:{}", blake3::hash(raw.as_bytes()).to_hex())
    );
    assert_eq!(
        evidence["attemptFamily"]["familyAlias"],
        crate::models::public_attempt_family_alias(FAMILY)
    );
    assert_eq!(evidence["attemptFamily"]["recordedSlots"], 2);
    assert_eq!(evidence["attemptFamily"]["selectedCount"], 1);
    assert_eq!(evidence["attemptFamily"]["rejectedCount"], 1);
    assert_eq!(evidence["attemptFamily"]["promotionPosture"], "eligible");
}

#[test]
fn payload_failure_rolls_back_memory_tags_jobs_and_audit() {
    let f = Fixture::new("decision");
    let (memory, mut payload) = f.capture();
    // Inject an invalid prepared sidecar to exercise a real setter failure
    // after INSERT, not a mocked transaction or a failure before publication.
    payload.typed_fields = Some("{invalid".to_owned());
    let (db, workspace) = f.destination();
    let before = durable_counts(&db);
    assert!(
        persist_global_promotion(&db, &workspace, &memory, &payload, None, chrono::Utc::now())
            .is_err()
    );
    assert_eq!(durable_counts(&db), before);
}

#[test]
fn later_audit_failure_rolls_back_the_typed_row_and_all_index_obligations() {
    let f = Fixture::new("decision");
    f.fields(json!({"chosen":"SQLite"}));
    let (memory, payload) = f.capture();
    let (db, workspace) = f.destination();
    let before = durable_counts(&db);
    db.execute_raw("ALTER TABLE audit_log RENAME TO unavailable_global_payload_audit")
        .unwrap();
    assert!(
        persist_global_promotion(&db, &workspace, &memory, &payload, None, chrono::Utc::now())
            .is_err()
    );
    db.execute_raw("ALTER TABLE unavailable_global_payload_audit RENAME TO audit_log")
        .unwrap();
    assert_eq!(durable_counts(&db), before);
    let (id, duplicate, _) = f.publish(&db, &workspace);
    assert!(!duplicate);
    assert_eq!(
        db.get_memory_typed_fields_json(&id).unwrap(),
        f.db.get_memory_typed_fields_json(MEMORY).unwrap()
    );
}
