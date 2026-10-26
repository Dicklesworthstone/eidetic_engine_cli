#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::error_diagnosis::{
    ErrorRepairLinkRecording, pack_error_recall_query_seed, record_error_repair_links,
};
use crate::core::error_recall::from_rustc;
use crate::db::{
    CreateMemoryInput, CreateProceduralRuleInput, CreateWorkspaceInput, StoredFeedbackQuarantine,
};

const WS: &str = "wsp_00000000000000000000000981";
const OTHER: &str = "wsp_00000000000000000000000982";
const BASE: &str = "2020-01-01T00:00:00Z";
const BODY: &str = "Implement Serialize for the cache payload to repair the trait bound failure.";
type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn fixture() -> TestResult<DbConnection> {
    let db = DbConnection::open_memory()?;
    db.migrate()?;
    for (id, path) in [(WS, "/native-rule-repair"), (OTHER, "/other-rule-repair")] {
        db.insert_workspace(
            id,
            &CreateWorkspaceInput {
                path: path.to_owned(),
                name: None,
            },
        )?;
    }
    Ok(db)
}

fn rule(db: &DbConnection, workspace: &str, number: u128, content: &str) -> TestResult<String> {
    let id = RuleId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
    db.insert_procedural_rule(
        &id,
        &CreateProceduralRuleInput {
            workspace_id: workspace.to_owned(),
            content: content.to_owned(),
            confidence: 0.9,
            utility: 0.8,
            importance: 0.7,
            trust_class: "human_explicit".to_owned(),
            scope: "workspace".to_owned(),
            scope_pattern: None,
            maturity: "validated".to_owned(),
            protected: true,
            source_memory_ids: Vec::new(),
            tags: Vec::new(),
        },
    )?;
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET created_at = '{BASE}', updated_at = '{BASE}' WHERE id = '{id}'"
    ))?;
    Ok(id)
}

fn link(db: &DbConnection, ids: Vec<String>) -> TestResult {
    record_error_repair_links(
        db,
        WS,
        &from_rustc(Some("E0277"), "trait bound failed"),
        &ErrorRepairLinkRecording {
            helpful_repairs: ids,
            ..Default::default()
        },
    )?;
    Ok(())
}

fn seed(db: &DbConnection) -> crate::db::Result<String> {
    pack_error_recall_query_seed(db, WS, &from_rustc(Some("E0277"), "trait bound failed"))
}

fn parent(db: &DbConnection, workspace: &str, number: u128, text: &str) -> TestResult<String> {
    let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
    db.insert_memory_with_timestamps(
        &id,
        &CreateMemoryInput {
            workspace_id: workspace.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: text.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.8,
            importance: 0.7,
            provenance_uri: Some("manual://rule-repair-lineage".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: Some(BASE.to_owned()),
            valid_to: None,
        },
        BASE,
        BASE,
        &id,
    )?;
    Ok(id)
}

fn hold(db: &DbConnection, workspace: &str, target: &str, number: u32) -> TestResult {
    db.insert_feedback_quarantine_for_recovery(&StoredFeedbackQuarantine {
        id: format!("fq_{number:026}"),
        workspace_id: workspace.to_owned(),
        source_id: "private-rule-repair-hold-source".to_owned(),
        target_type: "rule".to_owned(),
        target_id: target.to_owned(),
        signal: "harmful".to_owned(),
        weight: 0.5,
        source_type: "outcome_observed".to_owned(),
        proposed_event_id: Some(format!("fb_{number:026}")),
        recorded_at: BASE.to_owned(),
        reason: "private-rule-repair-hold-reason".to_owned(),
        event_reason: None,
        evidence_json: None,
        session_id: None,
        raw_event_hash: format!("blake3:{number:064x}"),
        status: "pending".to_owned(),
        reviewed_at: None,
        reviewed_by: None,
        released_feedback_event_id: None,
    })?;
    Ok(())
}

#[test]
fn linked_native_rule_enriches_public_error_pack_without_a_shadow_memory_or_write() -> TestResult {
    let db = fixture()?;
    let id = rule(&db, WS, 1, BODY)?;
    link(&db, vec![id.clone()])?;
    let before = db.get_procedural_rule(&id)?;
    let audits = db.count_table_rows("audit_log")?;
    let links = db.count_table_rows("error_repair_links")?;
    let actual = seed(&db)?;
    assert!(actual.contains(&format!("helpful_repair:{id}")));
    assert!(actual.contains(&format!("\nprior fix: {BODY}")));
    assert!(!actual.contains("helpful_repair:mem_"));
    assert_eq!(actual.matches("\nprior fix: ").count(), 1);
    assert_eq!(actual, seed(&db)?);
    assert_eq!(db.count_table_rows("memories")?, 0);
    assert_eq!(db.count_table_rows("audit_log")?, audits);
    assert_eq!(db.count_table_rows("error_repair_links")?, links);
    assert_eq!(db.get_procedural_rule(&id)?, before);
    Ok(())
}

#[test]
fn only_selected_current_native_rule_bodies_are_hydrated() -> TestResult {
    let db = fixture()?;
    let live = rule(&db, WS, 1, BODY)?;
    let retired = rule(&db, WS, 2, "Retired rule text.")?;
    let future = rule(&db, WS, 3, "Future rule text.")?;
    let oversized = rule(&db, WS, 4, &"x".repeat(MAX_SOURCE_BYTES + 1))?;
    let foreign = rule(&db, OTHER, 5, "Foreign rule text.")?;
    let held = rule(&db, WS, 6, "Held rule text.")?;
    let unlinked = rule(&db, WS, 7, "Unlinked rule text.")?;
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET tombstoned_at = '{BASE}' WHERE id = '{retired}'"
    ))?;
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET updated_at = '2999-01-01T00:00:00Z' WHERE id = '{future}'"
    ))?;
    hold(&db, WS, &held, 1)?;
    let mut loaded = Vec::new();
    let snapshot = super::super::memory::Snapshot::begin_or_borrow(&db)?;
    let actual = read_observed(
        &db,
        WS,
        &[&live, &live, &retired, &future, &oversized, &foreign, &held],
        Utc::now(),
        |ids| loaded = ids.iter().map(|id| (*id).to_owned()).collect(),
    )?;
    snapshot.finish()?;
    assert_eq!(loaded, std::slice::from_ref(&live));
    assert_eq!(actual, [(live, BODY.to_owned())]);
    assert!(!format!("{actual:?}").contains(&unlinked));
    Ok(())
}

#[test]
fn native_rule_edits_retirement_and_scope_changes_are_visible_without_reindexing() -> TestResult {
    let db = fixture()?;
    let id = rule(&db, WS, 1, BODY)?;
    link(&db, vec![id.clone()])?;
    assert!(seed(&db)?.contains(BODY));
    let changed = "Implement Deserialize for the cache payload before retrying the trait check.";
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET content = '{}' WHERE id = '{id}'",
        changed.replace('\'', "''")
    ))?;
    assert!(seed(&db)?.contains(changed));
    assert!(!seed(&db)?.contains(BODY));
    for maturity in ["draft", "deprecated", "superseded"] {
        db.execute_raw(&format!(
            "UPDATE procedural_rules SET maturity = '{maturity}' WHERE id = '{id}'"
        ))?;
        assert!(!seed(&db)?.contains(&id), "{maturity}");
    }
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET maturity = 'candidate' WHERE id = '{id}'"
    ))?;
    assert!(seed(&db)?.contains(changed));
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET scope = 'directory', scope_pattern = 'src' WHERE id = '{id}'"
    ))?;
    assert!(
        !seed(&db)?.contains(&id),
        "error text does not authorize a task path"
    );
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET scope = 'workspace', scope_pattern = NULL WHERE id = '{id}'"
    ))?;
    assert!(seed(&db)?.contains(changed));
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET tombstoned_at = '{BASE}' WHERE id = '{id}'"
    ))?;
    assert!(!seed(&db)?.contains(&id));
    Ok(())
}

#[test]
fn pending_rule_holds_override_protection_but_foreign_holds_do_not() -> TestResult {
    let db = fixture()?;
    let id = rule(&db, WS, 1, BODY)?;
    link(&db, vec![id.clone()])?;
    hold(&db, OTHER, &id, 1)?;
    assert!(seed(&db)?.contains(BODY));
    hold(&db, WS, &id, 2)?;
    let withheld = seed(&db)?;
    assert!(!withheld.contains(&id));
    assert!(!withheld.contains(BODY));
    assert!(!withheld.contains("private-rule-repair-hold"));
    db.execute_raw("UPDATE feedback_quarantine SET status = 'rejected', reviewed_at = '2020-01-01T00:00:00Z', reviewed_by = 'reviewer' WHERE id = 'fq_00000000000000000000000002'")?;
    assert!(seed(&db)?.contains(BODY));
    Ok(())
}

#[test]
fn lineage_ownership_is_required_without_inheriting_parent_retirement_or_body() -> TestResult {
    let db = fixture()?;
    let id = rule(&db, WS, 1, BODY)?;
    let own = parent(&db, WS, 11, "PRIVATE_PARENT_BODY must not be expanded.")?;
    let foreign = parent(&db, OTHER, 12, "FOREIGN_PARENT_BODY must not be expanded.")?;
    db.execute_raw(&format!(
        "INSERT INTO rule_source_memories (rule_id, memory_id) VALUES ('{id}', '{own}')"
    ))?;
    link(&db, vec![id.clone()])?;
    db.execute_raw(&format!(
        "UPDATE memories SET tombstoned_at = '{BASE}' WHERE id = '{own}'"
    ))?;
    let actual = seed(&db)?;
    assert!(actual.contains(BODY));
    assert!(!actual.contains("PRIVATE_PARENT_BODY"));
    assert!(!actual.contains(&own));
    db.execute_raw(&format!(
        "INSERT INTO rule_source_memories (rule_id, memory_id) VALUES ('{id}', '{foreign}')"
    ))?;
    let actual = seed(&db)?;
    assert!(!actual.contains(&id));
    assert!(!actual.contains(BODY));
    assert!(!actual.contains("FOREIGN_PARENT_BODY"));
    Ok(())
}

#[test]
fn full_body_screening_precedes_excerpting_and_typed_aliases_do_not_resolve() -> TestResult {
    let db = fixture()?;
    let safe = rule(&db, WS, 1, &format!("{BODY} {}", "🦀".repeat(400)))?;
    let unsafe_body = format!("{BODY} {} ghp_{}", "x".repeat(1000), "Q".repeat(36));
    let unsafe_rule = rule(&db, WS, 2, &unsafe_body)?;
    // Set original bytes after insertion as well: admission must handle
    // externally authored rows, not rely on any write-time screening.
    db.execute_raw(&format!(
        "UPDATE procedural_rules SET content = '{}' WHERE id = '{unsafe_rule}'",
        unsafe_body.replace('\'', "''")
    ))?;
    let aliased = rule(
        &db,
        WS,
        3,
        "ALIASED_RULE_TEXT is not selected by its real identity.",
    )?;
    link(
        &db,
        vec![
            safe.clone(),
            unsafe_rule.clone(),
            format!("{aliased}\0suffix"),
        ],
    )?;
    let actual = seed(&db)?;
    assert!(actual.contains(&safe));
    assert!(actual.contains(BODY));
    assert!(actual.contains(" [truncated]"));
    assert!(!actual.contains(&unsafe_rule));
    assert!(!actual.contains("ghp_"));
    assert!(!actual.contains("ALIASED_RULE_TEXT"));
    assert_eq!(actual.matches("\nprior fix: ").count(), 1);
    assert!(actual.len() <= super::super::PACK_RECALL_QUERY_BYTES);
    Ok(())
}

#[test]
fn rules_share_existing_excerpt_budget_and_deduplicate_with_native_memories() -> TestResult {
    let db = fixture()?;
    let memory = parent(&db, WS, 1, BODY)?;
    let duplicate = rule(&db, WS, 1, BODY)?;
    let mut targets = vec![memory.clone(), duplicate.clone()];
    let mut distinct = Vec::new();
    for number in 2..=7 {
        let text = format!(
            "Repair choice {number} implements the cache serialization trait before retrying."
        );
        let id = rule(&db, WS, number, &text)?;
        targets.push(id.clone());
        distinct.push((id, text));
    }
    link(&db, targets)?;
    let actual = seed(&db)?;
    assert!(actual.contains(&format!("helpful_repair:{memory}")));
    assert!(!actual.contains(&format!("helpful_repair:{duplicate}")));
    assert_eq!(
        actual.matches("\nprior fix: ").count(),
        super::super::PACK_RECALL_EXCERPT_LIMIT
    );
    for (id, text) in distinct.iter().take(3) {
        assert!(actual.contains(id));
        assert!(actual.contains(text));
    }
    for (id, _) in distinct.iter().skip(3) {
        assert!(!actual.contains(id));
    }
    assert!(actual.len() <= super::super::PACK_RECALL_QUERY_BYTES);
    assert_eq!(actual, seed(&db)?);
    Ok(())
}

#[test]
fn selected_relation_bounds_fail_without_returning_partial_advice() -> TestResult {
    let db = fixture()?;
    let id = rule(&db, WS, 1, BODY)?;
    link(&db, vec![id.clone()])?;
    assert!(seed(&db)?.contains(BODY));
    for number in 0..=MAX_RELATIONS {
        db.execute_raw(&format!(
            "INSERT INTO rule_tags (rule_id, tag) VALUES ('{id}', 'tag-{number:04}')"
        ))?;
    }
    let error = seed(&db)
        .expect_err("over-budget metadata must not return a partial query")
        .to_string();
    assert!(!error.contains(BODY));
    assert!(!error.contains(&id));
    assert!(!error.contains("tag-"));
    assert!(
        read(
            &db,
            WS,
            &vec![id.as_str(); PACK_RECALL_TARGET_LIMIT as usize + 1],
            Utc::now()
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn native_rule_expansion_preserves_caller_transaction_ownership() -> TestResult {
    let db = fixture()?;
    let id = rule(&db, WS, 1, BODY)?;
    link(&db, vec![id.clone()])?;
    let revised = "A temporary repair implements Serialize before the retry.";
    let outcome: crate::db::Result<()> = db.with_transaction(|| {
        db.execute_raw(&format!(
            "UPDATE procedural_rules SET content = '{}' WHERE id = '{id}'",
            revised.replace('\'', "''")
        ))?;
        assert!(seed(&db)?.contains(revised));
        db.execute_raw("INSERT INTO missing_rule_recall_rollback_table VALUES (1)")?;
        Ok(())
    });
    assert!(outcome.is_err());
    let restored = seed(&db)?;
    assert!(restored.contains(BODY));
    assert!(!restored.contains(revised));
    Ok(())
}
