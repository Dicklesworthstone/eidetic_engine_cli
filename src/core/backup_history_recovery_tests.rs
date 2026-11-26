//! Real restore/publication checks, including corruptions that preserve counts.

use super::tests::{fixture, recovery_agent_profile, recovery_feedback, recovery_rule};
use super::*;
use crate::models::{MemoryId, WorkspaceId};
use uuid::Uuid;

type TestResult = Result<(), String>;

fn learning_fixture(
    redaction: RedactionLevel,
) -> Result<(tempfile::TempDir, PathBuf, BackupRestoreOptions), String> {
    let (root, workspace, database) = fixture().map_err(|e| e.message())?;
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
    let memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    let rule = recovery_rule(&workspace_id, 0);
    db.with_transaction(|| {
        db.insert_procedural_rule_for_recovery(&rule)?;
        db.insert_procedural_rule_for_recovery(&recovery_rule(&workspace_id, 1))?;
        db.restore_rule_source(&rule.id, &memory_id)?;
        db.restore_rule_tag(&rule.id, "release")?;
        db.insert_feedback_event_for_recovery(&recovery_feedback(&workspace_id, &memory_id, 1))?;
        db.insert_agent_context_profile_for_recovery(&recovery_agent_profile(
            &workspace_id,
            &memory_id,
            "RecoveryAgent",
        ))
    })
    .map_err(|e| e.to_string())?;
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
    let restore = BackupRestoreOptions {
        workspace_path: workspace,
        backup_path: PathBuf::from(backup.backup_path),
        side_path: root.path().join("restored"),
        restore_graph_cache: false,
        dry_run: false,
    };
    Ok((root, database, restore))
}

#[test]
fn learned_content_survives_restore_and_rebackup_without_replaying_feedback() -> TestResult {
    for redaction in [
        RedactionLevel::None,
        RedactionLevel::Standard,
        RedactionLevel::Strict,
    ] {
        let (root, _source_database, mut options) = learning_fixture(redaction)?;
        for generation in 0..2 {
            let restored = restore_backup_to_side_path(&options).map_err(|e| e.message())?;
            assert_eq!(restored.restored_rule_count, 2);
            assert_eq!(restored.restored_rule_source_count, 1);
            assert_eq!(restored.restored_rule_tag_count, 1);
            assert_eq!(restored.restored_feedback_count, 1);
            assert_eq!(restored.restored_agent_profile_count, 1);
            let db = DbConnection::open_file(&restored.restored_database_path)
                .map_err(|e| e.to_string())?;
            let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
            let original = recovery_rule(&workspace_id, 0);
            assert_eq!(
                db.get_procedural_rule(&original.id)
                    .map_err(|e| e.to_string())?,
                Some(original)
            );
            let feedback = db
                .list_feedback_events(&workspace_id)
                .map_err(|e| e.to_string())?;
            assert_eq!(feedback.len(), 1);
            assert_eq!(
                feedback[0].applied_at.as_deref(),
                Some("2026-09-01T00:01:00Z")
            );
            assert_eq!(feedback[0].weight, 0.5);
            let profiles = db
                .list_agent_context_profiles_for_recovery(&workspace_id)
                .map_err(|e| e.to_string())?;
            assert_eq!(profiles.len(), 1);
            assert_eq!(profiles[0].weight_cached, 0.04);
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
                options.side_path = root.path().join("restored-again");
            }
        }
    }
    Ok(())
}

fn assert_same_count_corruption_refused(table: &str, sql: &str) -> TestResult {
    let (_root, source_database, options) = learning_fixture(RedactionLevel::None)?;
    let error = restore_backup_to_side_path_with_verification_hook(&options, |path| {
        let db = DbConnection::open_file(path).map_err(work_history_error)?;
        let tables = db.list_user_tables().map_err(work_history_error)?;
        let before = tables
            .iter()
            .map(|name| db.count_table_rows(name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(work_history_error)?;
        db.execute_raw(sql).map_err(work_history_error)?;
        let after = tables
            .iter()
            .map(|name| db.count_table_rows(name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(work_history_error)?;
        assert_eq!(
            before, after,
            "corruption must preserve every table's count"
        );
        db.close().map_err(work_history_error)
    })
    .err()
    .ok_or_else(|| format!("published changed {table} content"))?;
    assert!(
        error
            .message()
            .contains(&format!("Restored durable content differs for {table}")),
        "{}",
        error.message()
    );
    assert!(!error.message().contains("MUTATION_SENTINEL"));
    assert!(!error.message().contains("RecoveryAgent"));
    assert!(!options.side_path.join(WORKSPACE_MARKER).exists());
    let db = DbConnection::open_file(&source_database).map_err(|e| e.to_string())?;
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
    let original = recovery_rule(&workspace_id, 0);
    assert_eq!(
        db.get_procedural_rule(&original.id)
            .map_err(|e| e.to_string())?,
        Some(original)
    );
    db.close().map_err(|e| e.to_string())?;
    let verified = verify_backup(&BackupVerifyOptions {
        workspace_path: options.workspace_path,
        backup_path: options.backup_path,
    })
    .map_err(|e| e.message())?;
    assert_eq!(verified.status, "verified");
    Ok(())
}

#[test]
fn learned_content_fence_rejects_changed_rule_authority() -> TestResult {
    assert_same_count_corruption_refused(
        "procedural_rules",
        "UPDATE procedural_rules SET protected = 0, content = 'MUTATION_SENTINEL'",
    )
}

#[test]
fn learned_content_fence_rejects_reassigned_rule_evidence() -> TestResult {
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
    let other = recovery_rule(&workspace_id, 1).id;
    assert_same_count_corruption_refused(
        "rule_source_memories",
        &format!("UPDATE rule_source_memories SET rule_id = '{other}'"),
    )
}

#[test]
fn learned_content_fence_rejects_changed_rule_tags() -> TestResult {
    assert_same_count_corruption_refused(
        "rule_tags",
        "UPDATE rule_tags SET tag = 'MUTATION_SENTINEL'",
    )
}

#[test]
fn learned_content_fence_rejects_feedback_reapplication() -> TestResult {
    assert_same_count_corruption_refused(
        "feedback_events",
        "UPDATE feedback_events SET applied_at = NULL",
    )
}

#[test]
fn learned_content_fence_rejects_changed_agent_bias() -> TestResult {
    // Stay inside the +/-0.05 schema bound, so this exercises the publication
    // fence after a successful write rather than an earlier CHECK refusal.
    assert_same_count_corruption_refused(
        "agent_context_profiles",
        "UPDATE agent_context_profiles SET weight_cached = -0.04",
    )
}

mod rule_redaction_recovery {
    use super::*;
    use crate::models::{RuleMaturity, TrustClass};
    use crate::search::RuleIndexProjection;

    #[test]
    fn changed_live_rules_lose_positive_authority_without_raising_low_trust() -> TestResult {
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        for maturity in [
            RuleMaturity::Draft,
            RuleMaturity::Candidate,
            RuleMaturity::Validated,
        ] {
            for trust in TrustClass::all() {
                for confidence in [0.1, 0.95] {
                    let mut source = recovery_rule(&workspace_id, 0);
                    source.content = "api_key=rule-recovery-secret-canary".to_owned();
                    source.maturity = maturity.as_str().to_owned();
                    source.trust_class = trust.as_str().to_owned();
                    source.confidence = confidence;
                    assert_eq!(
                        prepare_recovery_rule(&source, RedactionLevel::None)
                            .map_err(|e| e.message())?,
                        (source.clone(), false)
                    );
                    for redaction in [
                        RedactionLevel::Minimal,
                        RedactionLevel::Standard,
                        RedactionLevel::Strict,
                        RedactionLevel::Paranoid,
                        RedactionLevel::Full,
                    ] {
                        let (actual, review_required) =
                            prepare_recovery_rule(&source, redaction).map_err(|e| e.message())?;
                        assert!(review_required);
                        assert_eq!(actual.content, "[REDACTED]");
                        assert_eq!(actual.maturity, "draft");
                        assert!(!actual.protected);
                        assert_eq!(actual.positive_feedback_count, 0);
                        assert_eq!(actual.validation_passes, 0);
                        assert!(actual.last_validated_at.is_none());
                        assert!(actual.last_applied_at.is_none());
                        let recovered_trust = actual
                            .trust_class
                            .parse::<TrustClass>()
                            .map_err(|e| e.to_string())?;
                        assert!(recovered_trust.initial_confidence() <= trust.initial_confidence());
                        assert!(
                            recovered_trust.initial_confidence()
                                <= TrustClass::AgentAssertion.initial_confidence()
                        );
                        assert!(actual.confidence <= source.confidence);
                        assert!(actual.confidence <= recovered_trust.initial_confidence());
                        if matches!(trust, TrustClass::CassEvidence | TrustClass::LegacyImport) {
                            assert_eq!(actual.trust_class, source.trust_class);
                        }
                        assert_eq!(
                            actual.negative_feedback_count,
                            source.negative_feedback_count
                        );
                        assert_eq!(
                            actual.validation_contradictions,
                            source.validation_contradictions
                        );
                        assert_eq!(actual.id, source.id);
                        assert_eq!(actual.workspace_id, source.workspace_id);
                        assert_eq!(actual.created_at, source.created_at);
                        assert_eq!(actual.updated_at, source.updated_at);
                        assert_eq!(actual.utility, source.utility);
                        assert_eq!(actual.importance, source.importance);
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn scope_changes_and_strict_truncation_also_require_review() -> TestResult {
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        let mut scoped = recovery_rule(&workspace_id, 0);
        scoped.scope = "file_pattern".to_owned();
        scoped.scope_pattern = Some("private/api_key=rule-scope-canary/**".to_owned());
        let (actual, review_required) =
            prepare_recovery_rule(&scoped, RedactionLevel::Standard).map_err(|e| e.message())?;
        assert!(review_required);
        assert_eq!(actual.content, scoped.content);
        assert_eq!(actual.scope_pattern.as_deref(), Some("[REDACTED]"));
        assert_eq!(actual.maturity, "draft");
        assert!(!actual.protected);

        let mut long = recovery_rule(&workspace_id, 1);
        long.content = format!(
            "{}Never publish without a signed review.",
            "Keep release evidence. ".repeat(20)
        );
        assert_eq!(
            prepare_recovery_rule(&long, RedactionLevel::Standard).map_err(|e| e.message())?,
            (long.clone(), false)
        );
        let (actual, review_required) =
            prepare_recovery_rule(&long, RedactionLevel::Strict).map_err(|e| e.message())?;
        assert!(review_required);
        assert_eq!(actual.content.chars().count(), 200);
        assert!(!actual.content.contains("Never publish"));
        assert_eq!(actual.maturity, "draft");
        assert!(!actual.protected);
        Ok(())
    }

    #[test]
    fn unchanged_rules_and_retired_authority_remain_exact() -> TestResult {
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        let source = recovery_rule(&workspace_id, 0);
        for redaction in [
            RedactionLevel::None,
            RedactionLevel::Minimal,
            RedactionLevel::Standard,
            RedactionLevel::Strict,
        ] {
            assert_eq!(
                prepare_recovery_rule(&source, redaction).map_err(|e| e.message())?,
                (source.clone(), false)
            );
        }
        for terminal_case in 0..4 {
            let mut terminal = source.clone();
            terminal.content = "api_key=retired-rule-secret-canary".to_owned();
            match terminal_case {
                0 => terminal.maturity = "deprecated".to_owned(),
                1 => terminal.maturity = "superseded".to_owned(),
                2 => terminal.superseded_by = Some(recovery_rule(&workspace_id, 1).id),
                _ => terminal.tombstoned_at = Some("2026-09-01T00:05:00Z".to_owned()),
            }
            let mut expected = terminal.clone();
            expected.content = "[REDACTED]".to_owned();
            assert_eq!(
                prepare_recovery_rule(&terminal, RedactionLevel::Standard)
                    .map_err(|e| e.message())?,
                (expected, false),
                "redaction must not reactivate retired or tombstoned rules"
            );
        }
        Ok(())
    }

    struct RuleRecoveryFixture {
        root: tempfile::TempDir,
        database: PathBuf,
        options: BackupRestoreOptions,
        source_rules: Vec<StoredProceduralRule>,
        source_feedback: Vec<StoredFeedbackEvent>,
    }

    fn capture_fixture(redaction: RedactionLevel) -> Result<RuleRecoveryFixture, String> {
        let (root, workspace, database) = fixture().map_err(|e| e.message())?;
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        let memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
        let mut source_rules = (0..4)
            .map(|n| recovery_rule(&workspace_id, n))
            .collect::<Vec<_>>();
        source_rules[0].content =
            "Keep release evidence in /Users/alice/private-release-evidence.".to_owned();
        source_rules[1].scope = "file_pattern".to_owned();
        source_rules[1].scope_pattern = Some("private/api_key=rule-scope-canary/**".to_owned());
        source_rules[3].content = "api_key=retired-rule-secret-canary".to_owned();
        source_rules[3].maturity = "deprecated".to_owned();
        for rule in &source_rules[..3] {
            assert!(
                RuleIndexProjection::new(rule.clone(), &workspace, Vec::new(), Vec::new())
                    .is_pack_admissible(),
                "live source controls must be pack-admissible before redaction"
            );
        }
        let mut source_feedback = Vec::new();
        for (n, signal) in [(1, "helpful"), (2, "harmful")] {
            let mut event = recovery_feedback(&workspace_id, &source_rules[0].id, n);
            event.target_type = "rule".to_owned();
            event.signal = signal.to_owned();
            event.source_id = Some(format!("reviewer-{n}"));
            event.reason = Some("Observed the original instructions before recovery.".to_owned());
            event.evidence_json = None;
            event.applied_at = Some("2026-09-01T00:01:00Z".to_owned());
            source_feedback.push(event);
        }
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        db.with_transaction(|| {
            for rule in &source_rules {
                db.insert_procedural_rule_for_recovery(rule)?;
                db.restore_rule_source(&rule.id, &memory_id)?;
                db.restore_rule_tag(&rule.id, "release")?;
            }
            for event in &source_feedback {
                db.insert_feedback_event_for_recovery(event)?;
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
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
        let warnings = backup
            .degraded
            .iter()
            .filter(|entry| entry.code == BACKUP_RULE_REDACTION_REVIEW_REQUIRED_CODE)
            .collect::<Vec<_>>();
        assert_eq!(
            warnings.len(),
            usize::from(redaction == RedactionLevel::Standard)
        );
        if let Some(warning) = warnings.first() {
            assert!(warning.message.contains("changed 2 active native rules"));
            assert!(!warning.message.contains("rule-scope-canary"));
            assert!(!warning.message.contains("/Users/alice"));
        }
        Ok(RuleRecoveryFixture {
            options: BackupRestoreOptions {
                workspace_path: workspace,
                backup_path: PathBuf::from(backup.backup_path),
                side_path: root.path().join("restored-rules"),
                restore_graph_cache: false,
                dry_run: false,
            },
            root,
            database,
            source_rules,
            source_feedback,
        })
    }

    fn assert_source_unchanged(source: &RuleRecoveryFixture) -> TestResult {
        let db = DbConnection::open_file(&source.database).map_err(|e| e.to_string())?;
        for original in &source.source_rules {
            assert_eq!(
                db.get_procedural_rule(&original.id)
                    .map_err(|e| e.to_string())?,
                Some(original.clone())
            );
        }
        let events = db
            .list_feedback_events(&source.source_rules[0].workspace_id)
            .map_err(|e| e.to_string())?;
        assert_eq!(events.len(), source.source_feedback.len());
        for original in &source.source_feedback {
            assert_eq!(
                events.iter().find(|event| event.id == original.id),
                Some(original)
            );
        }
        db.close().map_err(|e| e.to_string())
    }

    #[test]
    fn authenticated_redacted_rules_stay_drafts_across_recovery_generations() -> TestResult {
        for redaction in [RedactionLevel::None, RedactionLevel::Standard] {
            let source = capture_fixture(redaction)?;
            let mut options = source.options.clone();
            for generation in 0..2 {
                let needs_warning = generation == 0 && redaction == RedactionLevel::Standard;
                let inspected = inspect_backup(&BackupInspectOptions {
                    backup_path: options.backup_path.clone(),
                })
                .map_err(|e| e.message())?;
                assert_eq!(
                    inspected
                        .degraded
                        .iter()
                        .any(|entry| { entry.code == BACKUP_RULE_REDACTION_REVIEW_REQUIRED_CODE }),
                    needs_warning
                );
                assert_eq!(
                    verify_backup(&BackupVerifyOptions {
                        workspace_path: options.workspace_path.clone(),
                        backup_path: options.backup_path.clone(),
                    })
                    .map_err(|e| e.message())?
                    .status,
                    "verified"
                );
                let mut preview_options = options.clone();
                preview_options.dry_run = true;
                let preview =
                    restore_backup_to_side_path(&preview_options).map_err(|e| e.message())?;
                assert_eq!(
                    preview
                        .degraded
                        .iter()
                        .any(|entry| { entry.code == BACKUP_RULE_REDACTION_REVIEW_REQUIRED_CODE }),
                    needs_warning
                );
                assert!(!options.side_path.join(WORKSPACE_MARKER).exists());
                let restored = restore_backup_to_side_path(&options).map_err(|e| e.message())?;
                assert_eq!(restored.restored_rule_count, 4);
                assert_eq!(restored.restored_rule_source_count, 4);
                assert_eq!(restored.restored_rule_tag_count, 4);
                assert_eq!(restored.restored_feedback_count, 2);
                assert_eq!(
                    restored
                        .degraded
                        .iter()
                        .any(|entry| { entry.code == BACKUP_RULE_REDACTION_REVIEW_REQUIRED_CODE }),
                    needs_warning
                );
                let db = DbConnection::open_file(&restored.restored_database_path)
                    .map_err(|e| e.to_string())?;
                let workspace_id = &source.source_rules[0].workspace_id;
                let memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
                let sources = db
                    .list_rule_source_memory_ids_for_workspace(workspace_id)
                    .map_err(|e| e.to_string())?;
                let tags = db
                    .list_rule_tags_for_workspace(workspace_id)
                    .map_err(|e| e.to_string())?;
                for (index, original) in source.source_rules.iter().enumerate() {
                    let actual = db
                        .get_procedural_rule(&original.id)
                        .map_err(|e| e.to_string())?
                        .ok_or("recovered rule missing")?;
                    let changed_live = redaction == RedactionLevel::Standard && index < 2;
                    let mut expected = original.clone();
                    if redaction == RedactionLevel::Standard {
                        expected.content = redact_content(&original.content, redaction);
                        expected.scope_pattern = original
                            .scope_pattern
                            .as_deref()
                            .map(|pattern| redact_content(pattern, redaction));
                    }
                    if changed_live {
                        expected.maturity = "draft".to_owned();
                        expected.protected = false;
                        expected.trust_class = "agent_assertion".to_owned();
                        expected.confidence = 0.5;
                        expected.positive_feedback_count = 0;
                        expected.validation_passes = 0;
                        expected.last_applied_at = None;
                        expected.last_validated_at = None;
                    }
                    assert_eq!(actual, expected);
                    let rule_sources = sources
                        .get(&original.id)
                        .ok_or("source relationship lost")?;
                    let rule_tags = tags.get(&original.id).ok_or("rule tags lost")?;
                    // Standard redaction rewrites memory identifiers
                    // (redact_identifier), so the import mints a stable
                    // redacted id and the backup maps rule sources to it
                    // (backup_memory_id_mapping). Each generation re-mints,
                    // so the source must name the memory restored here.
                    let context = format!("redaction {redaction:?} generation {generation}");
                    assert_eq!(rule_sources.len(), 1, "{context}");
                    if redaction == RedactionLevel::None {
                        assert_eq!(rule_sources, &vec![memory_id.clone()], "{context}");
                    } else {
                        assert_ne!(rule_sources[0], memory_id, "{context}");
                        assert!(
                            db.get_memory(&rule_sources[0])
                                .map_err(|e| e.to_string())?
                                .is_some(),
                            "{context}: rule source must name a restored memory"
                        );
                    }
                    assert_eq!(rule_tags, &vec!["release".to_owned()]);
                    let projection = RuleIndexProjection::new(
                        actual,
                        &options.side_path,
                        rule_tags.clone(),
                        rule_sources.clone(),
                    );
                    assert_eq!(projection.is_pack_admissible(), index < 3 && !changed_live);
                    if changed_live {
                        assert!(projection.is_search_indexable(), "draft remains reviewable");
                    }
                }
                let events = db
                    .list_feedback_events(workspace_id)
                    .map_err(|e| e.to_string())?;
                assert_eq!(events.len(), source.source_feedback.len());
                for original in &source.source_feedback {
                    assert_eq!(
                        events.iter().find(|event| event.id == original.id),
                        Some(original),
                        "positive and harmful history keeps applied markers instead of replaying"
                    );
                }
                db.close().map_err(|e| e.to_string())?;
                assert_source_unchanged(&source)?;
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
                    assert!(
                        backup.degraded.iter().all(|entry| {
                            entry.code != BACKUP_RULE_REDACTION_REVIEW_REQUIRED_CODE
                        }),
                        "unchanged recovery drafts must not claim a second redaction"
                    );
                    options.workspace_path = options.side_path;
                    options.backup_path = PathBuf::from(backup.backup_path);
                    options.side_path = source.root.path().join("restored-rules-again");
                }
            }
        }
        Ok(())
    }

    #[test]
    fn final_publication_refuses_restoring_authority_to_a_redacted_rule() -> TestResult {
        let source = capture_fixture(RedactionLevel::Standard)?;
        let rule_id = &source.source_rules[0].id;
        let error = restore_backup_to_side_path_with_recovery_hooks(
            &source.options,
            |_| Ok(()),
            |path| {
                assert!(
                    path.parent()
                        .ok_or_else(|| work_history_error("missing staged parent"))?
                        .join("index/meta.json")
                        .is_file()
                );
                let db = DbConnection::open_file(path).map_err(work_history_error)?;
                let count = db.count_table_rows("procedural_rules").map_err(work_history_error)?;
                assert_eq!(
                    db.get_procedural_rule(rule_id)
                        .map_err(work_history_error)?
                        .ok_or_else(|| work_history_error("redacted rule missing"))?
                        .maturity,
                    "draft"
                );
                db.execute_raw(&format!(
                    "UPDATE procedural_rules SET maturity = 'validated', protected = 1 WHERE id = '{rule_id}'"
                ))
                .map_err(work_history_error)?;
                assert_eq!(
                    db.count_table_rows("procedural_rules").map_err(work_history_error)?,
                    count
                );
                db.close().map_err(work_history_error)
            },
        )
        .err()
        .ok_or("published restored authority for redacted instructions")?;
        assert!(
            error
                .message()
                .contains("content differs for procedural_rules"),
            "{}",
            error.message()
        );
        assert!(!source.options.side_path.join(WORKSPACE_MARKER).exists());
        assert_source_unchanged(&source)
    }
}

mod transcript_publication {
    use super::*;
    use crate::db::{
        CreateEvidenceSpanInput, CreateSessionInput, EvidenceProducerKind, StoredSession,
    };
    use crate::models::{EvidenceId, SessionId};

    struct TranscriptFixture {
        root: tempfile::TempDir,
        database: PathBuf,
        options: BackupRestoreOptions,
        workspace_id: String,
        source_session: StoredSession,
        source_spans: Vec<StoredEvidenceSpan>,
        expected_session: StoredSession,
        expected_spans: Vec<BackupCassEvidenceRecord>,
    }

    fn transcript_fixture() -> Result<TranscriptFixture, String> {
        let (root, workspace, database) = fixture().map_err(|e| e.message())?;
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        let session_id = SessionId::from_uuid(Uuid::from_u128(501)).to_string();
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        db.insert_session(
            &session_id,
            &CreateSessionInput {
                workspace_id: workspace_id.clone(),
                cass_session_id: "/portable-fixture/session.jsonl".to_owned(),
                source_path: Some("/portable-fixture/source.jsonl".to_owned()),
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 257,
                token_count: None,
                content_hash: hash_bytes(b"recovery transcript fixture"),
                metadata_json: None,
            },
        )
        .map_err(|e| e.to_string())?;
        for n in (1..=257_u32).rev() {
            let id = EvidenceId::from_uuid(Uuid::from_u128(u128::from(n))).to_string();
            let body = format!("Recovery message {n}: {}", "界".repeat(1024));
            db.insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: workspace_id.clone(),
                    session_id: session_id.clone(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("source-line-{n}"),
                    span_kind: "message".to_owned(),
                    start_line: n,
                    end_line: n,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    content_hash: hash_bytes(body.as_bytes()),
                    excerpt: body,
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .map_err(|e| e.to_string())?;
            if n % 2 == 0 {
                db.execute_raw(&format!("UPDATE evidence_spans SET search_eligibility = 'denied', pack_eligibility = 'denied' WHERE id = '{id}'")).map_err(|e| e.to_string())?;
            }
        }
        let source_session = db
            .get_session(&session_id)
            .map_err(|e| e.to_string())?
            .ok_or("session missing")?;
        let source_spans = db
            .list_evidence_spans_for_workspace(&workspace_id)
            .map_err(|e| e.to_string())?;
        let expected_session = BackupCassSessionRecord::from_stored(&source_session)
            .into_restored(workspace_id.clone());
        let expected_spans = source_spans
            .iter()
            .map(|span| {
                BackupCassEvidenceRecord::for_export(
                    span,
                    &workspace_id,
                    Some(&source_session),
                    RedactionLevel::None,
                )
            })
            .collect();
        db.close().map_err(|e| e.to_string())?;
        let backup = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            output_dir: None,
            label: None,
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        assert!(backup.recovery_inventory.snapshot_coverage_complete);
        let options = BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(backup.backup_path),
            side_path: root
                .path()
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join("restored-transcript"),
            restore_graph_cache: false,
            dry_run: false,
        };
        Ok(TranscriptFixture {
            root,
            database,
            options,
            workspace_id,
            source_session,
            source_spans,
            expected_session,
            expected_spans,
        })
    }

    fn assert_source_unchanged(source: &TranscriptFixture) -> TestResult {
        let db = DbConnection::open_file(&source.database).map_err(|e| e.to_string())?;
        assert_eq!(
            db.get_session(&source.source_session.id)
                .map_err(|e| e.to_string())?,
            Some(source.source_session.clone())
        );
        assert_eq!(
            db.list_evidence_spans_for_workspace(&source.workspace_id)
                .map_err(|e| e.to_string())?,
            source.source_spans
        );
        db.close().map_err(|e| e.to_string())
    }

    fn assert_restored(source: &TranscriptFixture, database: &Path) -> TestResult {
        let db = DbConnection::open_file(database).map_err(|e| e.to_string())?;
        assert_eq!(
            db.get_session(&source.expected_session.id)
                .map_err(|e| e.to_string())?,
            Some(source.expected_session.clone())
        );
        let actual = db
            .list_evidence_spans_for_workspace(&source.workspace_id)
            .map_err(|e| e.to_string())?;
        let records: Vec<_> = actual
            .iter()
            .map(BackupCassEvidenceRecord::from_stored)
            .collect();
        assert_eq!(records, source.expected_spans);
        assert_eq!(actual.len(), 257);
        assert_eq!(
            actual
                .iter()
                .filter(|row| row.pack_eligibility == "denied")
                .count(),
            128
        );
        assert_eq!(
            db.count_table_rows("sessions").map_err(|e| e.to_string())?,
            1
        );
        assert_eq!(
            db.count_table_rows("evidence_spans")
                .map_err(|e| e.to_string())?,
            257
        );
        db.close().map_err(|e| e.to_string())
    }

    #[test]
    fn multi_chunk_transcripts_survive_both_fences_and_a_second_recovery_generation() -> TestResult
    {
        let source = transcript_fixture()?;
        let first = restore_backup_to_side_path(&source.options).map_err(|e| e.message())?;
        assert_eq!(first.restored_cass_session_count, 1);
        assert_eq!(first.restored_evidence_span_count, 257);
        assert_restored(&source, Path::new(&first.restored_database_path))?;
        assert_source_unchanged(&source)?;
        let backup = create_backup(&BackupCreateOptions {
            workspace_path: source.options.side_path.clone(),
            database_path: Some(PathBuf::from(first.restored_database_path)),
            output_dir: None,
            label: None,
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        let second = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: source.options.side_path.clone(),
            backup_path: PathBuf::from(backup.backup_path),
            side_path: source
                .root
                .path()
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join("restored-transcript-again"),
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        assert_restored(&source, Path::new(&second.restored_database_path))?;
        assert_source_unchanged(&source)
    }

    fn refuse_late_transcript_change(table: &str, sql: &str) -> TestResult {
        let source = transcript_fixture()?;
        let mut staged = None;
        let error = restore_backup_to_side_path_with_recovery_hooks(
            &source.options,
            |_| Ok(()),
            |path| {
                assert!(
                    path.parent()
                        .ok_or_else(|| work_history_error("missing staged parent"))?
                        .join("index/meta.json")
                        .is_file()
                );
                staged = Some(path.to_path_buf());
                let db = DbConnection::open_file(path).map_err(work_history_error)?;
                let count = db.count_table_rows(table).map_err(work_history_error)?;
                db.execute_raw(sql).map_err(work_history_error)?;
                assert_eq!(
                    db.count_table_rows(table).map_err(work_history_error)?,
                    count
                );
                db.close().map_err(work_history_error)
            },
        )
        .err()
        .ok_or("published a modified transcript")?;
        assert!(
            error
                .message()
                .contains(&format!("content differs for {table}")),
            "{}",
            error.message()
        );
        assert!(!error.message().contains("PRIVATE_TRANSCRIPT_MUTATION"));
        assert!(!source.options.side_path.join(WORKSPACE_MARKER).exists());
        assert!(
            staged
                .ok_or("final publication hook did not execute")?
                .is_file()
        );
        assert_source_unchanged(&source)?;
        assert_eq!(
            verify_backup(&BackupVerifyOptions {
                workspace_path: source.options.workspace_path.clone(),
                backup_path: source.options.backup_path.clone(),
            })
            .map_err(|e| e.message())?
            .status,
            "verified"
        );
        let mut retry = source.options.clone();
        retry.side_path.set_file_name("restored-transcript-retry");
        let restored = restore_backup_to_side_path(&retry).map_err(|e| e.message())?;
        assert_restored(&source, Path::new(&restored.restored_database_path))
    }

    #[test]
    fn last_transcript_body_changed_after_rebuild_is_not_published() -> TestResult {
        let last = EvidenceId::from_uuid(Uuid::from_u128(257)).to_string();
        refuse_late_transcript_change(
            "evidence_spans",
            &format!(
                "UPDATE evidence_spans SET excerpt = 'PRIVATE_TRANSCRIPT_MUTATION' WHERE id = '{last}'"
            ),
        )
    }

    #[test]
    fn restored_session_cannot_regain_a_host_path_after_rebuild() -> TestResult {
        refuse_late_transcript_change(
            "sessions",
            "UPDATE sessions SET source_path = '/PRIVATE_TRANSCRIPT_MUTATION'",
        )
    }
}
