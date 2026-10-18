//! Cross-kind feedback admission through the actual error-guided pack query.

use super::tests::{OTHER_WS, TestResult, WS, db, session, span};
use super::*;
use crate::core::error_diagnosis::{
    ErrorRepairLinkRecording, error_recall_report, pack_error_recall_query_seed,
    record_error_repair_links,
};
use crate::core::error_recall::from_rustc;
use crate::db::{CreateMemoryInput, CreateProceduralRuleInput};
use crate::models::{MemoryId, RuleId};

const CODE: &str = "E0277";
const KEY: &str = "rustc:E0277";
const MEMORY_BODY: &str = "MEMORY_FIX implements Serialize for the cache payload.";
const RULE_BODY: &str = "RULE_FIX preserves the canonical serializer trait boundary.";
const GOOD_BODY: &str = "GOOD_FIX supplies the required trait implementation before retrying.";

fn record(connection: &DbConnection, helpful: Vec<String>, harmful: Vec<String>) -> TestResult {
    record_error_repair_links(
        connection,
        WS,
        &from_rustc(Some(CODE), "trait bound failed"),
        &ErrorRepairLinkRecording {
            helpful_repairs: helpful,
            harmful_repairs: harmful,
            ..Default::default()
        },
    )?;
    Ok(())
}

fn seed(connection: &DbConnection) -> crate::db::Result<String> {
    pack_error_recall_query_seed(
        connection,
        WS,
        &from_rustc(Some(CODE), "trait bound failed"),
    )
}

fn memory(connection: &DbConnection, number: u128, body: &str) -> TestResult<String> {
    let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
    connection.insert_memory_with_timestamps(
        &id,
        &CreateMemoryInput {
            workspace_id: WS.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: body.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.8,
            importance: 0.7,
            provenance_uri: Some("manual://feedback-recall".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        },
        "2000-01-01T00:00:00Z",
        "2000-01-01T00:00:00Z",
        &id,
    )?;
    Ok(id)
}

fn rule(connection: &DbConnection, number: u128) -> TestResult<String> {
    let id = RuleId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
    connection.insert_procedural_rule(
        &id,
        &CreateProceduralRuleInput {
            workspace_id: WS.to_owned(),
            content: RULE_BODY.to_owned(),
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
    Ok(id)
}

#[test]
fn harmful_feedback_excludes_evidence_memories_and_rules_without_erasing_history() -> TestResult {
    let connection = db()?;
    let session = session(&connection, WS, 920)?;
    let evidence = span(&connection, WS, &session, 1, false)?;
    let memory = memory(&connection, 2, MEMORY_BODY)?;
    let rule = rule(&connection, 3)?;
    let good = self::memory(&connection, 4, GOOD_BODY)?;
    let targets = vec![evidence.clone(), memory.clone(), rule.clone(), good.clone()];
    record(&connection, targets, Vec::new())?;
    let before = seed(&connection)?;
    for text in ["Repair 1", MEMORY_BODY, RULE_BODY, GOOD_BODY] {
        assert!(before.contains(text), "positive control: {text}");
    }
    record(
        &connection,
        Vec::new(),
        vec![evidence.clone(), memory.clone(), rule.clone()],
    )?;
    let rows = connection.list_error_repair_links(WS, KEY)?;
    let source = connection.get_procedural_rule(&rule)?;
    let after = seed(&connection)?;
    assert!(after.contains(GOOD_BODY));
    assert!(after.contains(&format!("helpful_repair:{good}")));
    assert_eq!(after.matches("\nprior fix: ").count(), 1);
    for text in [
        "Repair 1",
        MEMORY_BODY,
        RULE_BODY,
        evidence.as_str(),
        memory.as_str(),
        rule.as_str(),
    ] {
        assert!(
            !after.contains(text),
            "harmful target became positive guidance: {text}"
        );
    }
    assert_eq!(seed(&connection)?, after);
    assert_eq!(connection.list_error_repair_links(WS, KEY)?, rows);
    assert_eq!(connection.get_procedural_rule(&rule)?, source);
    let history = error_recall_report(
        &connection,
        WS,
        &from_rustc(Some(CODE), "trait bound failed"),
    )?;
    assert_eq!(history.helpful_repairs.len(), 4);
    assert_eq!(history.harmful_repairs.len(), 3);
    assert!(history.helpful_repairs.contains(&rule));
    assert!(history.harmful_repairs.contains(&rule));
    Ok(())
}

#[test]
fn selection_skips_a_harmful_prefix_before_the_shared_target_limit() -> TestResult {
    let connection = db()?;
    let session = session(&connection, WS, 921)?;
    let id = |number: u128| EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
    let bad: Vec<_> = (1..=128).map(id).collect();
    let good: Vec<_> = (129..=164).map(id).collect();
    for number in 129..=132 {
        span(&connection, WS, &session, number, false)?;
    }
    let mut all = bad.clone();
    all.extend(good.iter().cloned());
    record(&connection, all, bad.clone())?;
    let legacy = connection.list_helpful_error_repair_target_ids(WS, KEY, 32)?;
    assert_eq!(legacy.len(), 32);
    assert!(legacy.iter().all(|target| bad.contains(target)));
    let admitted = feedback::pack_targets(&connection, WS, KEY, 32)?;
    assert_eq!(admitted.as_slice(), &good[..32]);
    let query = seed(&connection)?;
    assert!(query.contains("Repair 129"));
    assert!(query.contains("Repair 132"));
    assert_eq!(query.matches("\nprior fix: ").count(), 4);
    assert!(query.len() <= super::super::PACK_RECALL_QUERY_BYTES);
    for target in bad {
        assert!(!query.contains(&target));
    }
    Ok(())
}

#[test]
fn selector_preserves_legacy_bounds_and_aliases_when_feedback_is_uncontested() -> TestResult {
    let connection = db()?;
    let good = memory(&connection, 1, GOOD_BODY)?;
    let alias = format!("{good}\0suffix");
    let oversized = format!("!{}", "x".repeat(16_384));
    record(
        &connection,
        vec![good.clone(), alias.clone(), oversized],
        Vec::new(),
    )?;
    let legacy = connection.list_helpful_error_repair_target_ids(WS, KEY, 32)?;
    let admitted = feedback::pack_targets(&connection, WS, KEY, 32)?;
    assert_eq!(admitted, legacy);
    assert!(admitted.iter().any(String::is_empty));
    assert!(admitted.contains(&alias));
    // Negative alias, foreign scope and another error class are not this target.
    record(&connection, Vec::new(), vec![alias])?;
    for (workspace, code) in [(OTHER_WS, CODE), (WS, "E0308")] {
        record_error_repair_links(
            &connection,
            workspace,
            &from_rustc(Some(code), "trait bound failed"),
            &ErrorRepairLinkRecording {
                harmful_repairs: vec![good.clone()],
                ..Default::default()
            },
        )?;
    }
    assert!(seed(&connection)?.contains(GOOD_BODY));
    record(&connection, Vec::new(), vec![good.clone()])?;
    let query = seed(&connection)?;
    assert!(!query.contains(GOOD_BODY));
    assert!(!query.contains(&good));
    Ok(())
}

#[test]
fn report_query_seed_retains_harm_warnings_without_contradictory_positive_hints() -> TestResult {
    let connection = db()?;
    record(
        &connection,
        vec!["mem_contested".to_owned(), "mem_good".to_owned()],
        vec!["mem_contested".to_owned()],
    )?;
    let report = error_recall_report(
        &connection,
        WS,
        &from_rustc(Some(CODE), "trait bound failed"),
    )?;
    assert_eq!(report.helpful_repairs.len(), 2);
    let query = report.query_seed();
    assert!(!query.contains("helpful_repair:mem_contested"));
    assert!(query.contains("harmful_repair:mem_contested"));
    assert!(query.contains("helpful_repair:mem_good"));
    Ok(())
}

#[test]
fn missing_feedback_table_never_falls_back_to_optimistic_pack_expansion() -> TestResult {
    let connection = db()?;
    let good = memory(&connection, 1, GOOD_BODY)?;
    record(&connection, vec![good], Vec::new())?;
    assert!(seed(&connection)?.contains(GOOD_BODY));
    connection.execute_raw("ALTER TABLE error_repair_links RENAME TO unavailable_pack_feedback")?;
    assert!(seed(&connection).is_err());
    connection.begin_read_snapshot()?;
    connection.rollback_read_snapshot()?;
    assert!(feedback::pack_targets(&connection, WS, KEY, 0)?.is_empty());
    assert!(feedback::pack_targets(&connection, WS, KEY, 33).is_err());
    Ok(())
}

#[test]
fn pack_feedback_admission_does_not_commit_or_rollback_its_callers_write() -> TestResult {
    let connection = db()?;
    let good = memory(&connection, 1, GOOD_BODY)?;
    record(&connection, vec![good.clone()], Vec::new())?;
    let before = seed(&connection)?;
    let outcome: crate::db::Result<()> = connection.with_transaction(|| {
        record_error_repair_links(
            &connection,
            WS,
            &from_rustc(Some(CODE), "trait bound failed"),
            &ErrorRepairLinkRecording {
                harmful_repairs: vec![good.clone()],
                ..Default::default()
            },
        )?;
        assert!(!seed(&connection)?.contains(GOOD_BODY));
        assert_eq!(connection.list_error_repair_links(WS, KEY)?.len(), 2);
        connection.execute_raw("INSERT INTO missing_pack_feedback_rollback VALUES (1)")?;
        Ok(())
    });
    assert!(outcome.is_err());
    assert_eq!(connection.list_error_repair_links(WS, KEY)?.len(), 1);
    assert_eq!(seed(&connection)?, before);
    Ok(())
}
