use std::path::{Path, PathBuf};

use super::*;
use crate::config::WORKSPACE_MARKER;
use crate::core::backup::{
    BackupCreateOptions, BackupRestoreOptions, BackupVerifyOptions, create_backup, hash_bytes,
    restore_backup_to_side_path, restore_backup_to_side_path_with_recovery_hooks, verify_backup,
    work_history_error,
};
use crate::db::{
    CreateCurationCandidateInput, StoredDebtSnapshot, StoredPlanRecipe,
    StoredReflectionRequestLedger, StoredSituationRecord, StoredTripwire, StoredTripwireCheckEvent,
};
use crate::models::{
    MemoryId, MemorySentinelPolarity, MemorySentinelSpec, RedactionLevel, StoredMemorySentinelSpec,
    WorkspaceId,
};
use serde_json::json;
use uuid::Uuid;

type TestResult = Result<(), String>;
const TIME: &str = "2026-09-01T00:00:00Z";

fn workspace_id() -> String {
    WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string()
}

fn memory_id() -> String {
    MemoryId::from_uuid(Uuid::from_u128(2)).to_string()
}

fn rows() -> Result<StoredMaintenanceHistory, String> {
    let workspace = workspace_id();
    let memory = memory_id();
    let hash = hash_bytes(b"maintenance fidelity");
    let spec = MemorySentinelSpec::from_raw(
        &memory,
        "path_exists:Cargo.toml",
        MemorySentinelPolarity::Gate,
        None,
        "api_key=maintenance-private-canary",
        Some(300),
    )
    .map_err(|e| e.to_string())?;
    Ok(StoredMaintenanceHistory {
        debt_snapshots: vec![StoredDebtSnapshot {
            workspace_id: workspace.clone(),
            snapshot_day: "2026-09-01".to_owned(),
            generation: 7,
            report_hash: hash.clone(),
            report_json: json!({"note":"api_key=maintenance-private-canary"}).to_string(),
            item_count: 3,
            total_score: 0.75,
            created_at: TIME.to_owned(),
        }],
        sentinel_specs: vec![StoredMemorySentinelSpec {
            spec_hash: spec.spec_hash,
            memory_id: memory.clone(),
            sentinel_kind: spec.sentinel_kind,
            polarity: spec.polarity,
            target: spec.target,
            expected_predicate: spec.expected_predicate,
            safety_class: spec.safety_class,
            provenance: spec.provenance,
            stale_threshold_seconds: spec.stale_threshold_seconds,
            created_at: TIME.to_owned(),
            updated_at: TIME.to_owned(),
        }],
        reflection_requests: vec![StoredReflectionRequestLedger {
            request_id: "reflect_req_fidelity".to_owned(),
            request_hash: hash.clone(),
            workspace_id: workspace.clone(),
            reflection_kind: "gaps".to_owned(),
            source_package_hash: hash.clone(),
            source_refs_json: json!([{"kind":"memory", "id":memory, "contentHash":hash}])
                .to_string(),
            source_content_hashes_json: json!([hash.clone()]).to_string(),
            prompt_template_hash: hash.clone(),
            response_schema_hash: hash.clone(),
            created_at: TIME.to_owned(),
            expires_at: "2099-01-01T00:00:00Z".to_owned(),
            challenge_key_id: "reflect_key_historical".to_owned(),
            challenge_hash: hash.clone(),
            status: "consumed".to_owned(),
            consumed_candidate_id: Some(format!("curate_{:026}", 91)),
            consumed_at: Some(TIME.to_owned()),
            consumed_result_hash: Some(hash.clone()),
        }],
        situations: vec![StoredSituationRecord {
            situation_id: "sit_fidelity".to_owned(),
            workspace_scope: workspace.clone(),
            schema_version: "ee.situation.record.v1".to_owned(),
            input_hash: hash.clone(),
            original_text_redacted: Some("api_key=maintenance-private-canary".to_owned()),
            category: "release".to_owned(),
            confidence: "high".to_owned(),
            confidence_score: 0.75,
            signals_json: "[]".to_owned(),
            alternative_categories_json: "[]".to_owned(),
            routing_decisions_json: "[]".to_owned(),
            context_hints_json: "[]".to_owned(),
            provenance_json: "[]".to_owned(),
            adopted_by: Some("reviewer".to_owned()),
            adoption_reason: None,
            created_at: TIME.to_owned(),
            adopted_at: TIME.to_owned(),
            classifier_algorithm: "heuristic_v1".to_owned(),
            classifier_version: "1".to_owned(),
            build_version: "0.15.2".to_owned(),
        }],
        tripwires: vec![StoredTripwire {
            id: "tw_fidelity".to_owned(),
            workspace_id: workspace.clone(),
            preflight_run_id: "pre_fidelity".to_owned(),
            tripwire_type: "custom".to_owned(),
            condition: r#"task_contains_any("release")"#.to_owned(),
            action: "halt".to_owned(),
            state: "triggered".to_owned(),
            message: Some("api_key=maintenance-private-canary".to_owned()),
            created_at: TIME.to_owned(),
            last_checked_at: Some(TIME.to_owned()),
            triggered_at: Some(TIME.to_owned()),
            updated_at: TIME.to_owned(),
        }],
        tripwire_checks: vec![StoredTripwireCheckEvent {
            id: "tchk_fidelity".to_owned(),
            workspace_id: workspace.clone(),
            tripwire_id: "tw_fidelity".to_owned(),
            preflight_run_id: "pre_fidelity".to_owned(),
            checked_at: TIME.to_owned(),
            event_payload_hash: hash,
            condition_result: "satisfied".to_owned(),
            check_result: "triggered".to_owned(),
            should_halt: true,
            dry_run: false,
            durable_mutation: true,
            mutation_posture: "committed".to_owned(),
            details: None,
            schema: "ee.tripwire.check.v1".to_owned(),
        }],
        recipes: vec![StoredPlanRecipe {
            id: "plrec_fidelity".to_owned(),
            workspace_id: workspace,
            name: "Check release signatures".to_owned(),
            when_to_use: "Before publishing".to_owned(),
            steps_json: json!(["api_key=maintenance-private-canary"]).to_string(),
            evidence_uris_json: json!([format!("ee://memory/{}", memory_id())]).to_string(),
            maturity: "promoted".to_owned(),
            confidence: 0.75,
            helpful_count: 17,
            harmful_count: 2,
            created_at: TIME.to_owned(),
            updated_at: TIME.to_owned(),
            last_recommended_at: Some(TIME.to_owned()),
        }],
    })
}

struct Fixture {
    root: tempfile::TempDir,
    database: PathBuf,
    options: BackupRestoreOptions,
}

fn read_state(db: &DbConnection) -> Result<StoredMaintenanceHistory, String> {
    db.maintenance_history_for_recovery(&workspace_id())
        .map_err(|e| e.to_string())
}

fn fixture(redaction: RedactionLevel) -> Result<Fixture, String> {
    let (root, workspace, database) =
        crate::core::backup::tests::fixture().map_err(|e| e.message())?;
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    db.insert_curation_candidate(
        &format!("curate_{:026}", 91),
        &CreateCurationCandidateInput {
            workspace_id: workspace_id(),
            candidate_type: "promote".to_owned(),
            target_memory_id: Some(memory_id()),
            proposed_content: None,
            proposed_confidence: Some(0.75),
            proposed_trust_class: None,
            source_type: "human_request".to_owned(),
            source_id: None,
            reason: "Reviewed source".to_owned(),
            confidence: 0.75,
            status: Some("pending".to_owned()),
            created_at: Some(TIME.to_owned()),
            ttl_expires_at: None,
            derivation_source_refs_json: None,
            derivation_metadata_json: None,
        },
    )
    .map_err(|e| e.to_string())?;
    let original = rows()?;
    db.with_transaction(|| db.insert_maintenance_history_for_recovery(&original))
        .map_err(|e| e.to_string())?;
    assert_eq!(read_state(&db)?, original);
    db.close().map_err(|e| e.to_string())?;
    let backup = create_backup(&BackupCreateOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        output_dir: None,
        label: None,
        redaction_level: redaction,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|e| e.message())?;
    assert!(backup.recovery_inventory.snapshot_coverage_complete);
    let options = BackupRestoreOptions {
        workspace_path: workspace,
        backup_path: PathBuf::from(backup.backup_path),
        side_path: root.path().join("restored"),
        restore_graph_cache: false,
        dry_run: false,
    };
    Ok(Fixture {
        root,
        database,
        options,
    })
}

fn corrupt(path: &Path, sql: &str) -> Result<(), DomainError> {
    let db = DbConnection::open_file(path).map_err(work_history_error)?;
    let original = read_state(&db).map_err(work_history_error)?;
    let tables = db.list_user_tables().map_err(work_history_error)?;
    let counts = tables
        .iter()
        .map(|t| db.count_table_rows(t))
        .collect::<Result<Vec<_>, _>>()
        .map_err(work_history_error)?;
    db.execute_raw(sql).map_err(work_history_error)?;
    assert_ne!(read_state(&db).map_err(work_history_error)?, original);
    let after = tables
        .iter()
        .map(|t| db.count_table_rows(t))
        .collect::<Result<Vec<_>, _>>()
        .map_err(work_history_error)?;
    assert_eq!(counts, after, "every table count must survive the mutation");
    db.close().map_err(work_history_error)
}

fn assert_refused(table: &str, sql: &str, late: bool) -> TestResult {
    let fixture = fixture(RedactionLevel::Standard)?;
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    let original = read_state(&source)?;
    source.close().map_err(|e| e.to_string())?;
    let error = restore_backup_to_side_path_with_recovery_hooks(
        &fixture.options,
        |path| if late { Ok(()) } else { corrupt(path, sql) },
        |path| if late { corrupt(path, sql) } else { Ok(()) },
    )
    .err()
    .ok_or("published changed operational history")?;
    assert!(
        error
            .message()
            .contains(&format!("Restored durable content differs for {table}")),
        "{}",
        error.message()
    );
    assert!(!error.message().contains("PRIVATE_MUTATION"));
    assert!(!error.message().contains("maintenance-private-canary"));
    assert!(!fixture.options.side_path.join(WORKSPACE_MARKER).exists());
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    assert_eq!(read_state(&source)?, original);
    source.close().map_err(|e| e.to_string())?;
    assert_eq!(
        verify_backup(&BackupVerifyOptions {
            workspace_path: fixture.options.workspace_path,
            backup_path: fixture.options.backup_path,
        })
        .map_err(|e| e.message())?
        .status,
        "verified"
    );
    Ok(())
}

#[test]
fn maintenance_state_survives_two_generations_without_replaying_consumption_or_checks() -> TestResult
{
    for redaction in [RedactionLevel::None, RedactionLevel::Standard] {
        let fixture = fixture(redaction)?;
        let mut options = fixture.options;
        for generation in 0..2 {
            let restored = restore_backup_to_side_path(&options).map_err(|e| e.message())?;
            let db = DbConnection::open_file(&restored.restored_database_path)
                .map_err(|e| e.to_string())?;
            let state = read_state(&db)?;
            assert_eq!(maintenance_row_lengths(&state), [1; 7]);
            assert_eq!(state.reflection_requests[0].status, "consumed");
            assert_eq!(
                state.reflection_requests[0].consumed_at.as_deref(),
                Some(TIME)
            );
            assert_eq!(state.tripwires[0].action, "halt");
            assert_eq!(state.tripwires[0].state, "triggered");
            assert!(state.tripwire_checks[0].should_halt);
            assert_eq!(
                (
                    state.recipes[0].helpful_count,
                    state.recipes[0].harmful_count
                ),
                (17, 2)
            );
            if redaction == RedactionLevel::None {
                assert_eq!(state, rows()?);
            } else {
                assert!(
                    !serde_json::to_string(&state)
                        .map_err(|e| e.to_string())?
                        .contains("maintenance-private-canary")
                );
            }
            db.close().map_err(|e| e.to_string())?;
            if generation == 0 {
                let backup = create_backup(&BackupCreateOptions {
                    workspace_path: options.side_path.clone(),
                    database_path: Some(PathBuf::from(restored.restored_database_path)),
                    output_dir: None,
                    label: None,
                    redaction_level: redaction,
                    include_derived: false,
                    include_graph_cache: false,
                    dry_run: false,
                })
                .map_err(|e| e.message())?;
                options.workspace_path = options.side_path;
                options.backup_path = PathBuf::from(backup.backup_path);
                options.side_path = fixture.root.path().join("restored-again");
            }
        }
    }
    Ok(())
}

#[test]
fn maintenance_fence_rejects_changed_debt_assessment() -> TestResult {
    assert_refused(
        "debt_snapshots",
        "UPDATE debt_snapshots SET total_score = 0.5",
        false,
    )
}

#[test]
fn maintenance_fence_rejects_changed_sentinel_validity() -> TestResult {
    assert_refused(
        "memory_sentinel_specs",
        "UPDATE memory_sentinel_specs SET stale_threshold_seconds = 600",
        false,
    )
}

#[test]
fn maintenance_fence_rejects_reflection_replay_resurrection() -> TestResult {
    assert_refused(
        "reflection_request_ledger",
        "UPDATE reflection_request_ledger SET status = 'pending', consumed_candidate_id = NULL, consumed_at = NULL, consumed_result_hash = NULL",
        false,
    )
}

#[test]
fn maintenance_fence_rejects_changed_situation_routing() -> TestResult {
    assert_refused(
        "situation_records",
        "UPDATE situation_records SET routing_decisions_json = '[\"PRIVATE_MUTATION\"]'",
        false,
    )
}

#[test]
fn maintenance_fence_rejects_weakened_tripwire_action() -> TestResult {
    assert_refused("tripwires", "UPDATE tripwires SET action = 'warn'", false)
}

#[test]
fn maintenance_fence_rejects_erased_halt_history() -> TestResult {
    assert_refused(
        "tripwire_check_events",
        "UPDATE tripwire_check_events SET should_halt = 0",
        false,
    )
}

#[test]
fn maintenance_fence_rejects_rewritten_recipe() -> TestResult {
    assert_refused(
        "plan_recipes",
        "UPDATE plan_recipes SET steps_json = '[\"PRIVATE_MUTATION\"]'",
        false,
    )
}

#[test]
fn maintenance_fence_rejects_disabled_tripwire_after_index_rebuilding() -> TestResult {
    assert_refused("tripwires", "UPDATE tripwires SET state = 'disarmed'", true)
}

#[test]
fn maintenance_fence_rejects_reflection_replay_after_index_rebuilding() -> TestResult {
    assert_refused(
        "reflection_request_ledger",
        "UPDATE reflection_request_ledger SET status = 'pending', consumed_candidate_id = NULL, consumed_at = NULL, consumed_result_hash = NULL",
        true,
    )
}
