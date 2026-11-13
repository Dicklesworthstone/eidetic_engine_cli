//! Real-store admission and nonmutation checks for the global promotion lane.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;
use crate::core::global_promotion::{
    BackflowOptions, BackflowSignal, DemoteGlobalOptions, backflow_global_feedback, demote_global,
    promote_global,
};
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

const WORKSPACE: &str = "wsp_00000000000000000000000071";
const MEMORY: &str = "mem_00000000000000000000000071";
const OTHER: &str = "mem_00000000000000000000000072";
const BODY: &str = "Use the deployment's documented rollback procedure.";
const FROM: &str = "2000-01-01T00:00:00Z";
const TO: &str = "2099-01-01T00:00:00Z";

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    database: PathBuf,
    global: GlobalStorePaths,
    db: DbConnection,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let physical = root.path().canonicalize().unwrap();
        let database = physical.join("workspace.db");
        let db = DbConnection::open_file(&database).unwrap();
        db.migrate().unwrap();
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: physical.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        db.insert_memory(MEMORY, &memory_input(WORKSPACE, BODY))
            .unwrap();
        Self {
            _root: root,
            global: GlobalStorePaths::from_root(&physical.join("global")),
            root: physical,
            database,
            db,
        }
    }

    fn options(&self, dry_run: bool) -> PromoteGlobalOptions<'_> {
        PromoteGlobalOptions {
            workspace_database_path: &self.database,
            memory_id: MEMORY,
            global_paths: &self.global,
            global_lane_available: true,
            actor: None,
            dry_run,
        }
    }

    fn update(&self, fields: &str) {
        self.db
            .execute_raw(&format!(
                "UPDATE memories SET {fields} WHERE id = '{MEMORY}'"
            ))
            .unwrap();
    }

    fn seal(&self) {
        self.db
            .insert_memory_seal(
                MEMORY,
                &crate::models::memory_seal_commitment(BODY.as_bytes()),
                FROM,
            )
            .unwrap();
    }
}

fn memory_input(workspace: &str, body: &str) -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: workspace.to_owned(),
        level: "procedural".to_owned(),
        kind: "rule".to_owned(),
        content: body.to_owned(),
        workflow_id: None,
        confidence: 0.9,
        utility: 0.5,
        importance: 0.5,
        provenance_uri: None,
        trust_class: "human_explicit".to_owned(),
        trust_subclass: None,
        tags: Vec::new(),
        valid_from: Some(FROM.to_owned()),
        valid_to: Some(TO.to_owned()),
    }
}

fn disk(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                result.insert(path.strip_prefix(root).unwrap().to_owned(), Vec::new());
                visit(root, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

fn review_hold(
    db: &DbConnection,
    number: u32,
    workspace: &str,
    target_type: &str,
    target: &str,
) -> String {
    let id = format!("fq_{number:026}");
    db.insert_feedback_quarantine(
        &id,
        &crate::db::CreateFeedbackQuarantineInput {
            workspace_id: workspace.to_owned(),
            source_id: "PRIVATE-PROMOTION-REVIEW-SOURCE".to_owned(),
            target_type: target_type.to_owned(),
            target_id: target.to_owned(),
            signal: "harmful".to_owned(),
            weight: 1.0,
            source_type: "outcome_observed".to_owned(),
            proposed_event_id: None,
            recorded_at: "2026-01-01T00:00:00Z".to_owned(),
            reason: "PRIVATE-PROMOTION-REVIEW-REASON".to_owned(),
            event_reason: None,
            evidence_json: None,
            session_id: None,
            raw_event_hash: format!("blake3:{}", "a".repeat(64)),
        },
    )
    .unwrap();
    id
}

fn assert_review_refusal(report: &PromotionReport, observed_twin: Option<&str>) {
    assert!(matches!(
        &report.plan.verdict,
        PromotionVerdict::Refuse {
            refusal: PromotionRefusal::ReviewPending
        }
    ));
    assert!(!report.executed);
    assert_eq!(report.already_promoted, observed_twin.is_some());
    assert_eq!(report.global_memory_id.as_deref(), observed_twin);
    assert!(report.index_job_id.is_none() && report.merge_candidate_id.is_none());
    assert_eq!(report.index_status, "not_applicable");
    assert_eq!(report.plan.audit_action, "memory.promote_global_refused");
    assert_eq!(
        report.plan.data_json()["detail"]["code"],
        "global_promotion_evidence_gate"
    );
    let rendered = report.data_json().to_string();
    assert!(!rendered.contains("PRIVATE-PROMOTION-REVIEW"));
    assert!(!rendered.contains(BODY));
}

#[test]
fn pending_source_review_cannot_escape_into_a_new_global_identity() {
    let fixture = Fixture::new();
    review_hold(&fixture.db, 1, WORKSPACE, "memory", MEMORY);
    // Even an unusable destination must not be inspected for a held source.
    std::fs::write(&fixture.global.root, b"not a directory").unwrap();
    let before = disk(&fixture.root);
    let source = fixture.db.get_memory(MEMORY).unwrap().unwrap();
    assert_eq!(source.trust_class, "human_explicit");
    for dry_run in [true, false] {
        assert_review_refusal(&promote_global(&fixture.options(dry_run)).unwrap(), None);
        assert_eq!(disk(&fixture.root), before);
        assert_eq!(fixture.db.get_memory(MEMORY).unwrap().unwrap(), source);
        assert_eq!(fixture.db.count_table_rows("audit_log").unwrap(), 0);
        assert_eq!(fixture.db.count_table_rows("search_index_jobs").unwrap(), 0);
    }
}

#[test]
fn source_review_requires_native_scope_and_every_pending_hold_to_be_resolved() {
    let fixture = Fixture::new();
    let foreign = "wsp_00000000000000000000000072";
    fixture
        .db
        .insert_workspace(
            foreign,
            &CreateWorkspaceInput {
                path: fixture.root.join("foreign").to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
    review_hold(&fixture.db, 1, foreign, "memory", MEMORY);
    review_hold(&fixture.db, 2, WORKSPACE, "rule", MEMORY);
    assert!(
        promote_global(&fixture.options(true))
            .unwrap()
            .plan
            .allowed()
    );
    let first = review_hold(&fixture.db, 3, WORKSPACE, "memory", MEMORY);
    let second = review_hold(&fixture.db, 4, WORKSPACE, "memory", MEMORY);
    assert_review_refusal(&promote_global(&fixture.options(true)).unwrap(), None);
    assert!(
        fixture
            .db
            .update_feedback_quarantine_status(&first, "released", Some("reviewer"), None)
            .unwrap()
    );
    assert_review_refusal(&promote_global(&fixture.options(false)).unwrap(), None);
    assert!(!fixture.global.root.exists());
    assert!(
        fixture
            .db
            .update_feedback_quarantine_status(&second, "rejected", Some("reviewer"), None)
            .unwrap()
    );
    let report = promote_global(&fixture.options(false)).unwrap();
    assert!(report.executed && report.plan.allowed() && !report.already_promoted);
    let global = DbConnection::open_file_read_only(&fixture.global.database_path).unwrap();
    let copy = global
        .get_memory(report.global_memory_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(copy.content, BODY);
    assert_eq!(
        fixture.db.count_table_rows("feedback_quarantine").unwrap(),
        4
    );
    assert_eq!(global.count_table_rows("feedback_quarantine").unwrap(), 0);
}

#[test]
fn promotion_source_review_uses_the_same_snapshot_as_the_body() {
    let fixture = Fixture::new();
    let reference = timestamp("2030-01-01T00:00:00Z").unwrap();
    let (memory, _, admitted) =
        load_source_with_boundary(&fixture.options(true), reference, || {
            review_hold(&fixture.db, 1, WORKSPACE, "memory", MEMORY);
            Ok(())
        })
        .unwrap();
    assert_eq!(memory.content, BODY);
    assert!(admitted.allowed());
    let (_, _, next) = load_source(&fixture.options(true), reference).unwrap();
    assert!(matches!(
        next.verdict,
        PromotionVerdict::Refuse {
            refusal: PromotionRefusal::ReviewPending
        }
    ));
    assert!(!fixture.global.root.exists());
}

#[test]
fn selected_global_twin_review_is_rechecked_without_mutation_or_duplicate_fallback() {
    let fixture = Fixture::new();
    let (global, workspace) =
        crate::core::global_store::open_or_create_global_store(&fixture.global).unwrap();
    let later = "mem_00000000000000000000000073";
    for id in [OTHER, later] {
        global
            .insert_memory(id, &memory_input(&workspace, BODY))
            .unwrap();
    }
    global
        .insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: fixture
                    .root
                    .join("foreign-global")
                    .to_string_lossy()
                    .into_owned(),
                name: None,
            },
        )
        .unwrap();
    review_hold(&global, 1, WORKSPACE, "memory", OTHER);
    review_hold(&global, 2, &workspace, "rule", OTHER);
    // An earlier clean preview is not authority for the later write.
    let preview = promote_global(&fixture.options(true)).unwrap();
    assert!(preview.plan.allowed() && preview.already_promoted);
    assert_eq!(preview.global_memory_id.as_deref(), Some(OTHER));
    let hold = review_hold(&global, 3, &workspace, "memory", OTHER);
    let before_disk = disk(&fixture.root);
    let source = fixture.db.get_memory(MEMORY).unwrap().unwrap();
    let target = global.get_memory(OTHER).unwrap().unwrap();
    let generation = global.get_workspace_generation(&workspace).unwrap();
    let before_counts = [
        "memories",
        "audit_log",
        "search_index_jobs",
        "curation_candidates",
        "feedback_quarantine",
        "feedback_events",
    ]
    .map(|table| (table, global.count_table_rows(table).unwrap()));
    assert_review_refusal(
        &promote_global(&fixture.options(true)).unwrap(),
        Some(OTHER),
    );
    assert_eq!(disk(&fixture.root), before_disk);
    assert_review_refusal(
        &promote_global(&fixture.options(false)).unwrap(),
        Some(OTHER),
    );
    assert_eq!(fixture.db.get_memory(MEMORY).unwrap().unwrap(), source);
    assert_eq!(fixture.db.count_table_rows("audit_log").unwrap(), 0);
    assert_eq!(global.get_memory(OTHER).unwrap().unwrap(), target);
    assert_eq!(
        global.get_workspace_generation(&workspace).unwrap(),
        generation
    );
    for (table, count) in before_counts {
        assert_eq!(global.count_table_rows(table).unwrap(), count, "{table}");
    }

    assert!(
        global
            .update_feedback_quarantine_status(&hold, "released", Some("reviewer"), None)
            .unwrap()
    );
    // Preserve the existing first-compatible-ID rule: a later held duplicate
    // does not make the earlier clean row unusable or change which row wins.
    review_hold(&global, 4, &workspace, "memory", later);
    let resumed = promote_global(&fixture.options(false)).unwrap();
    assert!(resumed.executed && resumed.plan.allowed() && resumed.already_promoted);
    assert_eq!(resumed.global_memory_id.as_deref(), Some(OTHER));
    assert_eq!(global.count_table_rows("memories").unwrap(), 2);
    assert_eq!(global.count_table_rows("search_index_jobs").unwrap(), 0);
    assert_eq!(global.count_table_rows("curation_candidates").unwrap(), 0);
    assert_eq!(global.get_memory(OTHER).unwrap().unwrap(), target);
}

#[test]
fn unavailable_review_authority_fails_closed_before_promotion_side_effects() {
    let fixture = Fixture::new();
    fixture
        .db
        .execute_raw("ALTER TABLE feedback_quarantine RENAME TO PRIVATE_REVIEW_AUTHORITY")
        .unwrap();
    let before = disk(&fixture.root);
    for dry_run in [true, false] {
        let error = promote_global(&fixture.options(dry_run)).unwrap_err();
        assert!(error.contains("pending review authority"));
        assert!(!error.contains("PRIVATE_REVIEW_AUTHORITY") && !error.contains(BODY));
        assert_eq!(disk(&fixture.root), before);
        assert!(!fixture.global.root.exists());
    }

    let fixture = Fixture::new();
    let (global, workspace) =
        crate::core::global_store::open_or_create_global_store(&fixture.global).unwrap();
    global
        .insert_memory(OTHER, &memory_input(&workspace, BODY))
        .unwrap();
    global
        .execute_raw("ALTER TABLE feedback_quarantine RENAME TO PRIVATE_REVIEW_AUTHORITY")
        .unwrap();
    let generation = global.get_workspace_generation(&workspace).unwrap();
    let target = global.get_memory(OTHER).unwrap().unwrap();
    for dry_run in [true, false] {
        let error = promote_global(&fixture.options(dry_run)).unwrap_err();
        assert!(!error.contains("PRIVATE_REVIEW_AUTHORITY") && !error.contains(BODY));
        assert_eq!(fixture.db.count_table_rows("audit_log").unwrap(), 0);
        assert_eq!(global.count_table_rows("memories").unwrap(), 1);
        assert_eq!(global.count_table_rows("audit_log").unwrap(), 0);
        assert_eq!(global.count_table_rows("search_index_jobs").unwrap(), 0);
        assert_eq!(global.count_table_rows("curation_candidates").unwrap(), 0);
        assert_eq!(
            global.get_workspace_generation(&workspace).unwrap(),
            generation
        );
        assert_eq!(global.get_memory(OTHER).unwrap().unwrap(), target);
    }
}

#[test]
fn promotion_preview_of_an_absent_global_store_is_bytewise_read_only() {
    let fixture = Fixture::new();
    let before = disk(&fixture.root);
    let report = promote_global(&fixture.options(true)).unwrap();
    assert!(report.plan.allowed() && !report.executed);
    assert!(report.global_memory_id.is_none() && report.index_job_id.is_none());
    assert_eq!(disk(&fixture.root), before);
    assert!(!fixture.global.root.exists());
}

#[test]
fn refused_sources_never_initialize_or_inspect_the_destination() {
    for (update, code) in [
        (
            "trust_class = 'agent_assertion'",
            "global_promotion_evidence_gate",
        ),
        (
            "tombstoned_at = '2001-01-01T00:00:00Z'",
            "global_promotion_tombstoned",
        ),
        (
            "superseded_at = '2001-01-01T00:00:00Z'",
            "global_promotion_superseded",
        ),
        (
            "valid_to = '2001-01-01T00:00:00Z'",
            "global_promotion_expired",
        ),
        (
            "valid_from = '2098-01-01T00:00:00Z'",
            "global_promotion_not_yet_valid",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.update(update);
        // A broken destination must not prevent a source-policy refusal.
        std::fs::write(&fixture.global.root, b"not a directory").unwrap();
        let before = disk(&fixture.root);
        let report = promote_global(&fixture.options(false)).unwrap();
        assert!(!report.executed && !report.plan.allowed());
        assert_eq!(report.plan.data_json()["detail"]["code"], code);
        assert!(report.global_memory_id.is_none());
        assert_eq!(disk(&fixture.root), before);
    }
    let fixture = Fixture::new();
    let before = disk(&fixture.root);
    let mut options = fixture.options(false);
    options.global_lane_available = false;
    assert_eq!(
        promote_global(&options).unwrap().plan.data_json()["detail"]["code"],
        "global_lane_unavailable"
    );
    assert_eq!(disk(&fixture.root), before);
}

#[test]
fn author_windows_use_inclusive_instants_and_validate_both_bounds() {
    let reference = timestamp("2030-01-01T00:00:00Z").unwrap();
    assert_eq!(
        lifecycle_refusal(
            Some("2029-12-31T19:00:00-05:00"),
            Some("2030-01-01T01:00:00+01:00"),
            None,
            reference
        )
        .unwrap(),
        None
    );
    assert_eq!(
        lifecycle_refusal(
            Some("2030-01-01T00:00:00.000000001Z"),
            None,
            None,
            reference
        )
        .unwrap(),
        Some(PromotionRefusal::NotYetValid)
    );
    assert_eq!(
        lifecycle_refusal(
            None,
            Some("2029-12-31T23:59:59.999999999Z"),
            None,
            reference
        )
        .unwrap(),
        Some(PromotionRefusal::Expired)
    );
    assert_eq!(
        lifecycle_refusal(None, None, Some(TO), reference).unwrap(),
        Some(PromotionRefusal::Superseded)
    );
    assert!(lifecycle_refusal(Some(TO), Some(FROM), None, reference).is_err());
    assert!(lifecycle_refusal(Some(TO), Some("PRIVATE_CANARY"), None, reference).is_err());
}

#[test]
fn seals_control_promotion_independently_of_body_spelling_and_explicit_reveal() {
    let fixture = Fixture::new();
    fixture.seal();
    let before = disk(&fixture.root);
    let closed = promote_global(&fixture.options(false)).unwrap();
    assert_eq!(
        closed.plan.data_json()["detail"]["code"],
        "global_promotion_sealed"
    );
    assert!(!closed.data_json().to_string().contains(BODY));
    assert_eq!(disk(&fixture.root), before);
    fixture.db.mark_memory_seal_revealed(MEMORY, FROM).unwrap();
    fixture.update(&format!(
        "content = '{}'",
        crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
    ));
    assert!(
        promote_global(&fixture.options(true))
            .unwrap()
            .plan
            .allowed()
    );
    fixture.update("superseded_at = '2001-01-01T00:00:00Z'");
    assert_eq!(
        promote_global(&fixture.options(false))
            .unwrap()
            .plan
            .data_json()["detail"]["code"],
        "global_promotion_superseded"
    );
    assert!(!fixture.global.root.exists());
}

#[test]
fn body_and_seal_authority_observe_one_snapshot_during_a_concurrent_write() {
    let fixture = Fixture::new();
    let reference = timestamp("2030-01-01T00:00:00Z").unwrap();
    let (captured, _, plan) = load_source_with_boundary(&fixture.options(true), reference, || {
        fixture.update("content = 'Changed during promotion.'");
        fixture.seal();
        Ok(())
    })
    .unwrap();
    assert_eq!(captured.content, BODY);
    assert!(plan.allowed());
    let (_, _, next) = load_source(&fixture.options(true), reference).unwrap();
    assert_eq!(
        next.data_json()["detail"]["code"],
        "global_promotion_sealed"
    );
    assert!(!fixture.global.root.exists());
}

#[test]
fn malformed_or_unavailable_authority_cannot_publish_or_leak_diagnostics() {
    for update in [
        "valid_to = 'PRIVATE_LIFECYCLE_CANARY'",
        "superseded_at = 'PRIVATE_LIFECYCLE_CANARY'",
    ] {
        let fixture = Fixture::new();
        fixture.update(update);
        let before = disk(&fixture.root);
        let error = promote_global(&fixture.options(false)).unwrap_err();
        assert!(!error.contains("PRIVATE_LIFECYCLE_CANARY") && !error.contains(BODY));
        assert_eq!(disk(&fixture.root), before);
    }
    let fixture = Fixture::new();
    fixture
        .db
        .execute_raw("ALTER TABLE memory_seals RENAME TO unavailable_seals")
        .unwrap();
    let error = promote_global(&fixture.options(false)).unwrap_err();
    assert!(error.contains("verify memory seal sidecar"));
    assert!(!fixture.global.root.exists());
    let mut options = fixture.options(false);
    let missing = fixture.root.join("missing.db");
    options.workspace_database_path = &missing;
    assert!(promote_global(&options).is_err());
    assert!(!missing.exists());
}

#[test]
fn successful_publication_preserves_author_expiry_and_source_body() {
    let fixture = Fixture::new();
    let source = fixture.db.get_memory(MEMORY).unwrap().unwrap();
    let promoted = promote_global(&fixture.options(false)).unwrap();
    assert!(promoted.executed);
    let db = DbConnection::open_file_read_only(&fixture.global.database_path).unwrap();
    let copy = db
        .get_memory(promoted.global_memory_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(copy.valid_from, source.valid_from);
    assert_eq!(copy.valid_to, source.valid_to);
    assert_eq!(copy.content, source.content);
    assert_eq!(copy.trust_class, source.trust_class);
    assert_eq!(fixture.db.get_memory(MEMORY).unwrap().unwrap(), source);
}

#[test]
fn duplicate_preview_is_read_only_and_does_not_accept_hidden_or_short_lived_twins() {
    let fixture = Fixture::new();
    let (global, workspace) =
        crate::core::global_store::open_or_create_global_store(&fixture.global).unwrap();
    global
        .insert_memory(OTHER, &memory_input(&workspace, BODY))
        .unwrap();
    let before = disk(&fixture.root);
    let report = promote_global(&fixture.options(true)).unwrap();
    assert_eq!(report.global_memory_id.as_deref(), Some(OTHER));
    assert!(!report.executed);
    assert_eq!(disk(&fixture.root), before);
    for update in [
        "valid_to = '2001-01-01T00:00:00Z'",
        "valid_to = '2098-01-01T00:00:00Z'",
        "valid_to = '2099-01-01T00:00:00Z', superseded_at = '2001-01-01T00:00:00Z'",
        "superseded_at = NULL, trust_class = 'agent_assertion'",
        "trust_class = 'human_explicit', kind = 'fact'",
    ] {
        global
            .execute_raw(&format!(
                "UPDATE memories SET {update} WHERE id = '{OTHER}'"
            ))
            .unwrap();
        assert!(
            promote_global(&fixture.options(true))
                .unwrap()
                .global_memory_id
                .is_none()
        );
    }
    global
        .execute_raw(&format!(
            "UPDATE memories SET kind = 'rule' WHERE id = '{OTHER}'"
        ))
        .unwrap();
    global
        .insert_memory_seal(
            OTHER,
            &crate::models::memory_seal_commitment(BODY.as_bytes()),
            FROM,
        )
        .unwrap();
    assert!(
        promote_global(&fixture.options(true))
            .unwrap()
            .global_memory_id
            .is_none()
    );
}

#[test]
fn demotion_and_feedback_previews_neither_create_stores_nor_write_existing_stores() {
    let fixture = Fixture::new();
    let missing_source = fixture.root.join("absent-workspace.db");
    let demote = DemoteGlobalOptions {
        workspace_database_path: &missing_source,
        global_memory_id: OTHER,
        global_paths: &fixture.global,
        actor: None,
        dry_run: true,
    };
    let feedback = BackflowOptions {
        workspace_database_path: &missing_source,
        source_workspace_id: None,
        global_memory_id: OTHER,
        global_paths: &fixture.global,
        signal: BackflowSignal::Helpful,
        weight: 0.03,
        actor: None,
        event_id: None,
        prompt_injection_guard: true,
        dry_run: true,
    };
    let before = disk(&fixture.root);
    assert!(demote_global(&demote).is_err());
    assert!(backflow_global_feedback(&feedback).is_err());
    assert_eq!(disk(&fixture.root), before);
    let (global, workspace) =
        crate::core::global_store::open_or_create_global_store(&fixture.global).unwrap();
    global
        .insert_memory(OTHER, &memory_input(&workspace, BODY))
        .unwrap();
    let before = disk(&fixture.root);
    assert!(!demote_global(&demote).unwrap().executed);
    assert!(!backflow_global_feedback(&feedback).unwrap().executed);
    assert_eq!(disk(&fixture.root), before);
    assert!(!missing_source.exists());
}

#[test]
fn wrong_workspace_targets_and_nonfinite_feedback_fail_before_writes() {
    let fixture = Fixture::new();
    let (global, _) =
        crate::core::global_store::open_or_create_global_store(&fixture.global).unwrap();
    global
        .insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: fixture.root.join("foreign").to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
    global
        .insert_memory(OTHER, &memory_input(WORKSPACE, BODY))
        .unwrap();
    let before = global.get_memory(OTHER).unwrap();
    let audits = global.count_table_rows("audit_log").unwrap();
    let feedback_events = global.count_table_rows("feedback_events").unwrap();
    for dry_run in [true, false] {
        assert!(
            demote_global(&DemoteGlobalOptions {
                workspace_database_path: &fixture.database,
                global_memory_id: OTHER,
                global_paths: &fixture.global,
                actor: None,
                dry_run,
            })
            .is_err()
        );
        for weight in [0.03, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                backflow_global_feedback(&BackflowOptions {
                    workspace_database_path: &fixture.database,
                    source_workspace_id: None,
                    global_memory_id: OTHER,
                    global_paths: &fixture.global,
                    signal: BackflowSignal::Helpful,
                    weight,
                    actor: None,
                    event_id: None,
                    prompt_injection_guard: true,
                    dry_run,
                })
                .is_err()
            );
        }
    }
    assert_eq!(global.get_memory(OTHER).unwrap(), before);
    assert_eq!(global.count_table_rows("audit_log").unwrap(), audits);
    assert_eq!(
        global.count_table_rows("feedback_events").unwrap(),
        feedback_events
    );
}

#[test]
fn source_body_typed_fields_and_family_gate_share_one_snapshot() {
    let fixture = Fixture::new();
    fixture
        .db
        .set_memory_typed_fields_json(MEMORY, Some(r#"{"action":"original action"}"#))
        .unwrap();
    let original = fixture.db.get_memory_typed_fields_json(MEMORY).unwrap();
    let reference = timestamp("2030-01-01T00:00:00Z").unwrap();
    let (memory, payload, plan) =
        load_source_with_boundary(&fixture.options(true), reference, || {
            fixture
                .db
                .set_memory_typed_fields_json(MEMORY, Some(r#"{"action":"concurrent action"}"#))
                .unwrap();
            fixture
                .db
                .set_memory_attempt_family(
                    MEMORY,
                    &crate::db::MemoryAttemptFamily {
                        family_id: "private-concurrent-family".to_owned(),
                        declared_size: Some(2),
                        attempt_index: Some(1),
                        disposition: Some("selected".to_owned()),
                    },
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    assert!(plan.allowed());
    assert!(
        payload
            .matches_fields(&memory.kind, original.as_deref())
            .unwrap()
    );
    assert!(payload.audit_evidence()["attemptFamily"].is_null());
    let (_, _, next) = load_source(&fixture.options(true), reference).unwrap();
    assert!(!next.allowed());
    assert_eq!(
        next.data_json()["detail"]["code"],
        "global_promotion_evidence_gate"
    );
    assert!(
        !next
            .data_json()
            .to_string()
            .contains("private-concurrent-family")
    );
    assert!(!fixture.global.root.exists());
}
