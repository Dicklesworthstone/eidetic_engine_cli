//! Destination-store review and replay regressions for approximate promotion.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use super::*;
use crate::core::curate::{
    CurateApplyOptions, CurateShowOptions, CurateValidateOptions, apply_curation_candidate,
    show_curation_candidate, validate_curation_candidate,
};
use crate::core::global_promotion::{PromoteGlobalOptions, PromotionReport, promote_global};
use crate::core::global_store::{GlobalStorePaths, open_or_create_global_store};
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

const SOURCE: &str = "mem_00000000000000000000000081";
const TARGET: &str = "mem_00000000000000000000000082";
const WORKSPACE: &str = "wsp_00000000000000000000000081";
const BODY: &str =
    "Run cargo fmt before every release; inspect the complete test output before publishing.";
const NEAR_BODY: &str =
    "Run cargo fmt before every release;  inspect the complete test output before publishing.";
const FROM: &str = "2000-01-01T00:00:00Z";
const TO: &str = "2099-01-01T00:00:00Z";

struct Fixture {
    _root: tempfile::TempDir,
    source_path: PathBuf,
    source: DbConnection,
    paths: GlobalStorePaths,
    global: DbConnection,
    global_workspace: String,
}

fn input(workspace: &str, content: &str) -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: workspace.to_owned(),
        level: "procedural".to_owned(),
        kind: "rule".to_owned(),
        content: content.to_owned(),
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

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let physical = root.path().canonicalize().unwrap();
        let source_path = physical.join("source.db");
        let source = DbConnection::open_file(&source_path).unwrap();
        source.migrate().unwrap();
        source
            .insert_workspace(
                WORKSPACE,
                &CreateWorkspaceInput {
                    path: physical.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .unwrap();
        source
            .insert_memory(SOURCE, &input(WORKSPACE, NEAR_BODY))
            .unwrap();
        let paths = GlobalStorePaths::from_root(&physical.join("global"));
        let (global, global_workspace) = open_or_create_global_store(&paths).unwrap();
        global
            .insert_memory(TARGET, &input(&global_workspace, BODY))
            .unwrap();
        Self {
            _root: root,
            source_path,
            source,
            paths,
            global,
            global_workspace,
        }
    }

    fn promote(&self, dry_run: bool) -> PromotionReport {
        promote_global(&PromoteGlobalOptions {
            workspace_database_path: &self.source_path,
            memory_id: SOURCE,
            global_paths: &self.paths,
            global_lane_available: true,
            actor: Some("reviewer"),
            dry_run,
        })
        .unwrap()
    }

    fn candidate(&self, id: &str) -> StoredCurationCandidate {
        self.global
            .get_curation_candidate(&self.global_workspace, id)
            .unwrap()
            .unwrap()
    }

    fn update_target(&self, fields: &str) {
        self.global
            .execute_raw(&format!(
                "UPDATE memories SET {fields} WHERE id = '{TARGET}'"
            ))
            .unwrap();
    }

    fn apply_options<'a>(&'a self, id: &'a str) -> CurateApplyOptions<'a> {
        CurateApplyOptions {
            workspace_path: &self.paths.root,
            database_path: Some(&self.paths.database_path),
            candidate_id: id,
            actor: Some("reviewer"),
            dry_run: false,
            allow_tombstone_load_bearing: false,
        }
    }
}

#[test]
fn preview_and_retry_queue_one_global_review_without_mutating_memories_or_indexes() {
    let f = Fixture::new();
    let before_source = f.source.get_memory(SOURCE).unwrap().unwrap();
    let before_target = f.global.get_memory(TARGET).unwrap().unwrap();
    let before_audits = f.global.count_table_rows("audit_log").unwrap();
    let preview = f.promote(true);
    assert!(!preview.executed && !preview.already_promoted);
    assert_eq!(
        preview.data_json()["plan"]["detail"]["action"],
        "propose_merge"
    );
    assert_eq!(preview.global_memory_id.as_deref(), Some(TARGET));
    assert_eq!(f.global.count_table_rows("curation_candidates").unwrap(), 0);
    assert_eq!(
        f.global.count_table_rows("audit_log").unwrap(),
        before_audits
    );
    assert_eq!(f.source.count_table_rows("audit_log").unwrap(), 0);

    let report = f.promote(false);
    assert!(report.executed && !report.already_promoted);
    assert_eq!(report.merge_candidate_id, preview.merge_candidate_id);
    assert!(report.index_job_id.is_none());
    assert_eq!(report.index_status, "not_applicable");
    let id = report.merge_candidate_id.as_deref().unwrap();
    let candidate = f.candidate(id);
    assert_eq!(candidate.workspace_id, f.global_workspace);
    assert_eq!(candidate.target_memory_id.as_deref(), Some(TARGET));
    assert_eq!(candidate.status, "pending");
    assert_eq!(
        candidate.proposed_content,
        Some(format!("{BODY}\n\n{NEAR_BODY}"))
    );
    assert!(candidate.source_id.as_deref().unwrap().contains(SOURCE));
    assert!(candidate.source_id.as_deref().unwrap().contains(WORKSPACE));
    assert!(candidate.proposed_trust_class.is_none());
    validate_merge_candidate(&f.global, &candidate).unwrap();
    let command = report.review_command.as_deref().unwrap();
    assert!(command.contains(&format!("ee curate show {id}")));
    assert!(command.contains(f.paths.database_path.to_str().unwrap()));
    assert!(command.contains("--workspace"));
    let show = show_curation_candidate(&CurateShowOptions {
        workspace_path: &f.paths.root,
        database_path: Some(&f.paths.database_path),
        candidate_id: id,
    })
    .unwrap();
    assert_eq!(show.candidate.id, id);
    assert!(!show.durable_mutation);

    let retry = f.promote(false);
    assert_eq!(retry.merge_candidate_id.as_deref(), Some(id));
    assert_eq!(f.global.count_table_rows("curation_candidates").unwrap(), 1);
    assert_eq!(
        f.global.count_table_rows("audit_log").unwrap(),
        before_audits + 1
    );
    assert_eq!(f.global.count_table_rows("search_index_jobs").unwrap(), 0);
    assert_eq!(f.global.get_memory(TARGET).unwrap().unwrap(), before_target);
    assert_eq!(f.source.get_memory(SOURCE).unwrap().unwrap(), before_source);
}

#[test]
fn approved_merge_updates_existing_global_only_and_completed_repromotion_is_idempotent() {
    let f = Fixture::new();
    let report = f.promote(false);
    let id = report.merge_candidate_id.as_deref().unwrap();
    let validation = validate_curation_candidate(&CurateValidateOptions {
        workspace_path: &f.paths.root,
        database_path: Some(&f.paths.database_path),
        candidate_id: id,
        actor: Some("reviewer"),
        dry_run: false,
    })
    .unwrap();
    assert_eq!(validation.validation.decision, "approved");
    let applied = apply_curation_candidate(&f.apply_options(id)).unwrap();
    assert_eq!(applied.application.status, "applied");
    assert!(applied.durable_mutation);
    let target = f.global.get_memory(TARGET).unwrap().unwrap();
    assert_eq!(target.content, format!("{BODY}\n\n{NEAR_BODY}"));
    assert_eq!(target.trust_class, "human_explicit");
    assert_eq!(target.provenance_uri, None);
    assert_eq!(target.valid_from.as_deref(), Some(FROM));
    assert_eq!(target.valid_to.as_deref(), Some(TO));
    assert_eq!(
        f.source.get_memory(SOURCE).unwrap().unwrap().content,
        NEAR_BODY
    );
    assert_eq!(f.global.count_table_rows("memories").unwrap(), 1);
    let jobs = f.global.count_table_rows("search_index_jobs").unwrap();
    assert!(jobs > 0);
    assert!(
        f.paths.index_dir.is_dir(),
        "publish into the global indexes directory"
    );
    assert!(!f.paths.root.join(".ee/index").exists());
    validate_merge_candidate(&f.global, &f.candidate(id)).unwrap();

    let replay = f.promote(false);
    assert!(replay.already_promoted);
    assert_eq!(replay.merge_candidate_id.as_deref(), Some(id));
    assert_eq!(replay.data_json()["plan"]["detail"]["action"], "merge_into");
    let reapplied = apply_curation_candidate(&f.apply_options(id)).unwrap();
    assert!(!reapplied.durable_mutation);
    assert_eq!(f.global.count_table_rows("curation_candidates").unwrap(), 1);
    assert_eq!(
        f.global.count_table_rows("search_index_jobs").unwrap(),
        jobs
    );
    assert_eq!(f.global.get_memory(TARGET).unwrap().unwrap(), target);
}

#[test]
fn proposal_guard_rejects_changed_authority_and_stale_candidate_under_writer() {
    let f = Fixture::new();
    let report = f.promote(false);
    let id = report.merge_candidate_id.as_deref().unwrap();
    let candidate = f.candidate(id);
    for (changed, restored) in [
        ("kind = 'note'", "kind = 'rule'"),
        (
            "trust_class = 'agent_validated'",
            "trust_class = 'human_explicit'",
        ),
        (
            "valid_from = '2001-01-01T00:00:00Z'",
            "valid_from = '2000-01-01T00:00:00Z'",
        ),
        (
            "valid_to = '2098-01-01T00:00:00Z'",
            "valid_to = '2099-01-01T00:00:00Z'",
        ),
        (
            "superseded_at = '2020-01-01T00:00:00Z'",
            "superseded_at = NULL",
        ),
        ("typed_fields_json = '{}'", "typed_fields_json = NULL"),
    ] {
        f.update_target(changed);
        assert!(
            validate_merge_candidate(&f.global, &candidate).is_err(),
            "{changed}"
        );
        f.update_target(restored);
        validate_merge_candidate(&f.global, &candidate).unwrap();
    }
    let mut tampered = candidate.clone();
    tampered.proposed_content = Some("An unrelated replacement instruction.".to_owned());
    assert!(validate_merge_candidate(&f.global, &tampered).is_err());
    f.global
        .execute_raw(&format!(
            "UPDATE curation_candidates SET reason = 'Changed review context' WHERE id = '{id}'"
        ))
        .unwrap();
    assert!(validate_merge_candidate(&f.global, &candidate).is_err());
    f.global
        .insert_memory_seal(
            TARGET,
            &crate::models::memory_seal_commitment(BODY.as_bytes()),
            FROM,
        )
        .unwrap();
    assert!(validate_merge_candidate(&f.global, &f.candidate(id)).is_err());
    assert!(f.promote(true).merge_candidate_id.is_none());
    assert_eq!(f.global.count_table_rows("search_index_jobs").unwrap(), 0);
}

#[test]
fn incompatible_authority_is_not_an_approximate_merge_and_exact_twins_take_priority() {
    let f = Fixture::new();
    for (changed, restored) in [
        ("kind = 'note'", "kind = 'rule'"),
        (
            "trust_class = 'agent_validated'",
            "trust_class = 'human_explicit'",
        ),
        (
            "valid_from = '2001-01-01T00:00:00Z'",
            "valid_from = '2000-01-01T00:00:00Z'",
        ),
        ("valid_to = NULL", "valid_to = '2099-01-01T00:00:00Z'"),
        (
            "valid_to = '2001-01-01T00:00:00Z'",
            "valid_to = '2099-01-01T00:00:00Z'",
        ),
    ] {
        f.update_target(changed);
        let preview = f.promote(true);
        assert!(preview.merge_candidate_id.is_none(), "{changed}");
        assert_eq!(preview.data_json()["plan"]["detail"]["action"], "insert");
        f.update_target(restored);
    }
    f.global
        .insert_memory(
            "mem_00000000000000000000000083",
            &input(&f.global_workspace, NEAR_BODY),
        )
        .unwrap();
    let exact = f.promote(true);
    assert!(exact.already_promoted);
    assert!(exact.merge_candidate_id.is_none());
    assert_eq!(exact.data_json()["plan"]["detail"]["action"], "merge_into");
    assert_eq!(f.global.count_table_rows("curation_candidates").unwrap(), 0);
}

#[test]
fn changed_scope_tags_and_new_review_holds_block_pending_merge() {
    let f = Fixture::new();
    let report = f.promote(false);
    let id = report.merge_candidate_id.as_deref().unwrap();
    let candidate = f.candidate(id);
    f.global
        .add_memory_tags(TARGET, &["scope:private".to_owned()])
        .unwrap();
    assert!(validate_merge_candidate(&f.global, &candidate).is_err());
    f.global
        .remove_memory_tags(TARGET, &["scope:private".to_owned()])
        .unwrap();
    validate_merge_candidate(&f.global, &candidate).unwrap();
    f.global
        .insert_feedback_quarantine(
            "fq_00000000000000000000000081",
            &crate::db::CreateFeedbackQuarantineInput {
                workspace_id: f.global_workspace.clone(),
                source_id: "review-source".to_owned(),
                target_type: "memory".to_owned(),
                target_id: TARGET.to_owned(),
                signal: "harmful".to_owned(),
                weight: 1.0,
                source_type: "outcome_observed".to_owned(),
                proposed_event_id: None,
                recorded_at: "2026-01-01T00:00:00Z".to_owned(),
                reason: "Awaiting explicit review".to_owned(),
                event_reason: None,
                evidence_json: None,
                session_id: None,
                raw_event_hash: format!("blake3:{}", "a".repeat(64)),
            },
        )
        .unwrap();
    assert!(validate_merge_candidate(&f.global, &candidate).is_err());
    assert!(f.promote(true).merge_candidate_id.is_none());
    assert_eq!(f.global.get_memory(TARGET).unwrap().unwrap().content, BODY);
    assert_eq!(f.global.count_table_rows("search_index_jobs").unwrap(), 0);
}

#[test]
fn global_apply_keeps_durable_success_and_retries_the_same_job_in_the_global_index() {
    let f = Fixture::new();
    let report = f.promote(false);
    let id = report.merge_candidate_id.as_deref().unwrap();
    validate_curation_candidate(&CurateValidateOptions {
        workspace_path: &f.paths.root,
        database_path: Some(&f.paths.database_path),
        candidate_id: id,
        actor: Some("reviewer"),
        dry_run: false,
    })
    .unwrap();
    std::fs::write(&f.paths.index_dir, b"global publication blocker").unwrap();
    let first = apply_curation_candidate(&f.apply_options(id)).unwrap();
    assert!(first.durable_mutation);
    assert_eq!(first.application.status, "applied");
    assert!(first.degraded.iter().any(|entry| {
        entry.code == "curate_apply_index_publish_failed"
            && entry.repair.contains("--database")
            && entry.repair.contains(id)
    }));
    let candidate = f.candidate(id);
    let job_id = merge_index_job_id(&candidate).unwrap();
    let job = f.global.get_search_index_job(&job_id).unwrap().unwrap();
    assert_ne!(job.status, "completed");
    assert_eq!(job.document_id.as_deref(), Some(TARGET));
    assert_eq!(job.document_source.as_deref(), Some("memory"));
    assert!(!f.paths.root.join(".ee/index").exists());
    let target = f.global.get_memory(TARGET).unwrap().unwrap();
    std::fs::rename(
        &f.paths.index_dir,
        f.paths.root.join("preserved-publication-blocker"),
    )
    .unwrap();

    let retry = apply_curation_candidate(&f.apply_options(id)).unwrap();
    assert_eq!(retry.application.status, "already_applied");
    assert!(!retry.durable_mutation);
    assert!(retry.degraded.is_empty(), "{:?}", retry.degraded);
    assert!(f.paths.index_dir.is_dir());
    assert!(!f.paths.root.join(".ee/index").exists());
    assert_eq!(
        f.global
            .get_search_index_job(&job_id)
            .unwrap()
            .unwrap()
            .status,
        "completed"
    );
    assert_eq!(f.global.count_table_rows("search_index_jobs").unwrap(), 1);
    assert_eq!(f.global.get_memory(TARGET).unwrap().unwrap(), target);
    let promoted_again = f.promote(false);
    assert!(promoted_again.already_promoted);
    assert_eq!(
        promoted_again.index_job_id.as_deref(),
        Some(job_id.as_str())
    );
    assert_eq!(promoted_again.index_status, "indexed");
}

#[test]
fn failed_destination_audit_rolls_back_proposal_and_keeps_origin_untouched() {
    let f = Fixture::new();
    // Deliberately damage only this isolated fixture's audit table to force a
    // failure after candidate insertion and exercise the transaction boundary.
    f.global.execute_raw("DROP TABLE audit_log").unwrap();
    let result = promote_global(&PromoteGlobalOptions {
        workspace_database_path: &f.source_path,
        memory_id: SOURCE,
        global_paths: &f.paths,
        global_lane_available: true,
        actor: None,
        dry_run: false,
    });
    assert!(result.is_err());
    assert_eq!(f.global.count_table_rows("curation_candidates").unwrap(), 0);
    assert_eq!(f.global.count_table_rows("search_index_jobs").unwrap(), 0);
    assert_eq!(f.global.get_memory(TARGET).unwrap().unwrap().content, BODY);
    assert_eq!(f.source.count_table_rows("audit_log").unwrap(), 0);
}
