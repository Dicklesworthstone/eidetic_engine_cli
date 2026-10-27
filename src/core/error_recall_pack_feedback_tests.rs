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
    // Every target expected in the admitted window must have a live source.
    // Missing-source refill has separate regressions in error_recall_feedback.
    for number in 129..=164 {
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
    // History preserves exact metadata. The useful pack window no longer
    // spends slots on values none of its source readers can resolve.
    assert_eq!(admitted, std::slice::from_ref(&good));
    assert!(legacy.iter().any(String::is_empty));
    assert!(legacy.contains(&alias));
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

#[test]
fn unavailable_native_memory_windows_refill_with_later_current_repairs() -> TestResult {
    let connection = db()?;
    let mut targets = Vec::new();
    for number in 1..=192_u128 {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        if number % 6 != 0 {
            memory(
                &connection,
                number,
                "UNUSABLE_NATIVE_SENTINEL is not current repair guidance.",
            )?;
            match number % 6 {
                1 => connection.execute_raw(&format!(
                    "UPDATE memories SET valid_to = '2001-01-01T00:00:00Z' WHERE id = '{id}'"
                ))?,
                2 => connection.execute_raw(&format!(
                    "UPDATE memories SET workspace_id = '{OTHER_WS}' WHERE id = '{id}'"
                ))?,
                3 => connection.insert_memory_seal(
                    &id,
                    &format!("blake3:{}", "a".repeat(64)),
                    "2000-01-01T00:00:00Z",
                )?,
                4 => connection.execute_raw(&format!(
                    "UPDATE memories SET superseded_at = '2001-01-01T00:00:00Z' WHERE id = '{id}'"
                ))?,
                _ => connection.execute_raw(&format!(
                    "UPDATE memories SET valid_from = '2999-01-01T00:00:00Z' WHERE id = '{id}'"
                ))?,
            }
        }
        targets.push(id);
    }
    let good = memory(&connection, 1000, GOOD_BODY)?;
    let other_body = "SECOND_NATIVE_FIX flushes the cache before the serialization retry.";
    let other = memory(&connection, 1001, other_body)?;
    targets.extend([good.clone(), other.clone()]);
    record(&connection, targets.clone(), Vec::new())?;
    assert_eq!(
        connection.list_helpful_error_repair_target_ids(WS, KEY, 32)?,
        targets[..32]
    );
    let history = connection.list_error_repair_links(WS, KEY)?;
    let before = connection.get_memory(&good)?;
    let selection = feedback::pack_selection(&connection, WS, KEY, 32, chrono::Utc::now())?;
    assert_eq!(selection.targets, [good.clone(), other.clone()]);
    assert_eq!(selection.memories.len(), 2);
    assert_eq!(selection.memory_hints.len(), 4);
    let query = seed(&connection)?;
    assert!(query.contains(GOOD_BODY));
    assert!(query.contains(other_body));
    assert!(!query.contains("UNUSABLE_NATIVE_SENTINEL"));
    assert_eq!(query.matches("\nprior fix: ").count(), 2);
    assert!(query.matches("helpful_repair:").count() <= 6);
    assert_eq!(query, seed(&connection)?);
    assert_eq!(connection.get_memory(&good)?, before);
    assert_eq!(connection.list_error_repair_links(WS, KEY)?, history);
    Ok(())
}

#[test]
fn duplicate_native_memory_excerpts_do_not_hide_later_distinct_fixes() -> TestResult {
    let connection = db()?;
    let shared = format!(
        "Shared serialization repair preserves the cache. {}",
        "資料 🦀 ".repeat(300)
    );
    let mut targets = Vec::new();
    for number in 1..=96_u128 {
        // Full bodies differ, but their final bounded reader excerpts agree.
        targets.push(memory(
            &connection,
            number,
            &format!("{shared} Tail number {number}."),
        )?);
    }
    let representative = targets[0].clone();
    let mut distinct = Vec::new();
    for number in 1000..1003_u128 {
        let text =
            format!("Distinct native repair {number} restores the serializer before retrying.");
        let id = memory(&connection, number, &text)?;
        targets.push(id.clone());
        distinct.push((id, text));
    }
    record(&connection, targets, Vec::new())?;
    let selected = feedback::pack_selection(&connection, WS, KEY, 32, chrono::Utc::now())?;
    assert_eq!(
        selected.targets,
        std::iter::once(representative.clone())
            .chain(distinct.iter().map(|(id, _)| id.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(selected.memories.len(), 4);
    assert!(selected.memories[0].1.ends_with(" [truncated]"));
    let query = seed(&connection)?;
    assert_eq!(query.matches("\nprior fix: ").count(), 4);
    assert!(query.contains(&representative));
    for (id, text) in distinct {
        assert!(query.contains(&id));
        assert!(query.contains(&text));
    }
    assert!(query.len() <= super::super::PACK_RECALL_QUERY_BYTES);
    assert_eq!(query, seed(&connection)?);
    Ok(())
}

#[test]
fn missing_retired_foreign_and_path_scoped_rule_windows_do_not_hide_live_advice() -> TestResult {
    let connection = db()?;
    let mut targets = Vec::new();
    for number in 1..=128_u128 {
        let id = RuleId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        if number % 4 != 0 {
            rule(&connection, number)?;
            match number % 4 {
                1 => connection.execute_raw(&format!("UPDATE procedural_rules SET maturity = 'deprecated' WHERE id = '{id}'"))?,
                2 => connection.execute_raw(&format!("UPDATE procedural_rules SET workspace_id = '{OTHER_WS}' WHERE id = '{id}'"))?,
                _ => connection.execute_raw(&format!("UPDATE procedural_rules SET scope = 'directory', scope_pattern = 'src' WHERE id = '{id}'"))?,
            }
        }
        targets.push(id);
    }
    let good = rule(&connection, 1000)?;
    targets.push(good.clone());
    record(&connection, targets, Vec::new())?;
    let history = connection.list_error_repair_links(WS, KEY)?;
    let selected = feedback::pack_selection(&connection, WS, KEY, 32, chrono::Utc::now())?;
    assert_eq!(selected.targets, std::slice::from_ref(&good));
    assert_eq!(selected.rules, [(good.clone(), RULE_BODY.to_owned())]);
    assert!(selected.memory_hints.is_empty());
    let query = seed(&connection)?;
    assert_eq!(query.matches("\nprior fix: ").count(), 1);
    assert_eq!(query.matches("helpful_repair:").count(), 1);
    assert!(query.contains(&good));
    assert!(query.contains(RULE_BODY));
    assert_eq!(connection.list_error_repair_links(WS, KEY)?, history);
    Ok(())
}

#[test]
fn duplicate_rule_windows_refill_without_displacing_native_memory_precedence() -> TestResult {
    let connection = db()?;
    let first = memory(&connection, 1, RULE_BODY)?;
    let mut targets = vec![first.clone()];
    let mut duplicates = Vec::new();
    for number in 1..=96_u128 {
        let id = rule(&connection, number)?;
        targets.push(id.clone());
        duplicates.push(id);
    }
    let mut distinct = Vec::new();
    for number in 1000..1003_u128 {
        let id = rule(&connection, number)?;
        let text = format!("Distinct rule repair {number} preserves the serialization boundary.");
        connection.execute_raw(&format!(
            "UPDATE procedural_rules SET content = '{text}' WHERE id = '{id}'"
        ))?;
        targets.push(id.clone());
        distinct.push((id, text));
    }
    record(&connection, targets, Vec::new())?;
    let selected = feedback::pack_selection(&connection, WS, KEY, 32, chrono::Utc::now())?;
    assert_eq!(selected.memories.len(), 1);
    assert_eq!(selected.rules.len(), 4);
    let query = seed(&connection)?;
    assert!(query.contains(&first));
    assert_eq!(query.matches(RULE_BODY).count(), 1);
    assert_eq!(query.matches("\nprior fix: ").count(), 4);
    for id in duplicates {
        assert!(!query.contains(&id));
    }
    for (id, text) in distinct {
        assert!(query.contains(&id));
        assert!(query.contains(&text));
    }
    assert_eq!(query, seed(&connection)?);
    Ok(())
}

#[test]
fn every_native_target_limit_refills_and_retains_only_selected_bounded_excerpts() -> TestResult {
    let connection = db()?;
    let mut targets = (1..=40_u128)
        .map(|number| MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string())
        .collect::<Vec<_>>();
    let mut expected = Vec::new();
    for number in 0..36_u128 {
        let text = format!("Native repair group {number} restores the cache before retrying.");
        let first = memory(&connection, 1000 + 2 * number, &text)?;
        let copy = memory(&connection, 1001 + 2 * number, &text)?;
        expected.push(first.clone());
        targets.extend([first, copy]);
    }
    record(&connection, targets, Vec::new())?;
    let at = chrono::Utc::now();
    connection.begin_read_snapshot()?;
    for limit in 0..=32_u32 {
        let selected = feedback::pack_selection(&connection, WS, KEY, limit, at)?;
        assert_eq!(selected.targets, expected[..limit as usize]);
        assert_eq!(
            selected
                .memories
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            expected[..limit as usize]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert!(
            selected
                .memories
                .iter()
                .all(|(_, text)| text.len() <= super::super::PACK_RECALL_EXCERPT_BYTES)
        );
        assert!(selected.rules.is_empty());
        assert!(selected.memory_hints.len() <= 4);
    }
    connection.commit_read_snapshot()?;
    let unavailable = DbConnection::open_memory()?;
    let empty = feedback::pack_selection(&unavailable, WS, KEY, 0, at)?;
    assert!(empty.targets.is_empty() && empty.memories.is_empty() && empty.memory_hints.is_empty());
    assert!(feedback::pack_selection(&unavailable, WS, KEY, 33, at).is_err());
    Ok(())
}

#[test]
fn complete_source_refusals_precede_native_diversity_and_do_not_reserve_a_safe_excerpt()
-> TestResult {
    let connection = db()?;
    let prefix = format!(
        "The serializer repair preserves checked cache keys. {}",
        "資料 🦀 ".repeat(250)
    );
    let sealed = memory(&connection, 1, &prefix)?;
    connection.insert_memory_seal(
        &sealed,
        &format!("blake3:{}", "a".repeat(64)),
        "2000-01-01T00:00:00Z",
    )?;
    let harmful = memory(&connection, 2, &prefix)?;
    let poisoned = memory(&connection, 3, &prefix)?;
    let poison = format!("{prefix} api_key=ghp_{}", "Q".repeat(36));
    connection.execute_raw(&format!(
        "UPDATE memories SET content = '{}' WHERE id = '{poisoned}'",
        poison.replace('\'', "''")
    ))?;
    let good = memory(&connection, 1000, &prefix)?;
    let good_rule = rule(&connection, 1000)?;
    record(
        &connection,
        vec![
            sealed.clone(),
            harmful.clone(),
            poisoned.clone(),
            good.clone(),
            good_rule.clone(),
        ],
        vec![harmful.clone()],
    )?;
    let history = connection.list_error_repair_links(WS, KEY)?;
    let selected = feedback::pack_selection(&connection, WS, KEY, 32, chrono::Utc::now())?;
    assert_eq!(selected.targets, [good.clone(), good_rule.clone()]);
    assert_eq!(selected.memories.len(), 1);
    assert_eq!(selected.rules.len(), 1);
    assert!(!selected.memory_hints.contains(&harmful));
    let query = seed(&connection)?;
    assert!(query.contains(&good));
    assert!(query.contains(&good_rule));
    assert!(query.contains(" [truncated]"));
    assert!(!query.contains("ghp_") && !query.contains("api_key="));
    assert!(!query.contains(&harmful));
    assert_eq!(query.matches("\nprior fix: ").count(), 2);
    assert_eq!(connection.list_error_repair_links(WS, KEY)?, history);
    Ok(())
}

#[test]
fn native_refill_rechecks_changed_eligibility_and_preserves_caller_rollback() -> TestResult {
    let connection = db()?;
    let first = memory(&connection, 1, MEMORY_BODY)?;
    let later = memory(&connection, 1000, GOOD_BODY)?;
    let retained_rule = rule(&connection, 1000)?;
    let mut targets = (2..=96_u128)
        .map(|number| MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string())
        .collect::<Vec<_>>();
    targets.extend([first.clone(), later.clone(), retained_rule.clone()]);
    record(&connection, targets, Vec::new())?;
    let before = seed(&connection)?;
    assert!(
        before.contains(MEMORY_BODY) && before.contains(GOOD_BODY) && before.contains(RULE_BODY)
    );
    let history = connection.list_error_repair_links(WS, KEY)?;
    let transaction: crate::db::Result<()> = connection.with_transaction(|| {
        connection.execute_raw(&format!(
            "UPDATE memories SET valid_to = '2001-01-01T00:00:00Z' WHERE id = '{first}'"
        ))?;
        connection.execute_raw(&format!(
            "UPDATE procedural_rules SET maturity = 'deprecated' WHERE id = '{retained_rule}'"
        ))?;
        let selected = feedback::pack_selection(&connection, WS, KEY, 1, chrono::Utc::now())?;
        assert_eq!(selected.targets, std::slice::from_ref(&later));
        let inside = seed(&connection)?;
        assert!(inside.contains(GOOD_BODY));
        assert!(!inside.contains(MEMORY_BODY) && !inside.contains(RULE_BODY));
        connection.execute_raw("INSERT INTO missing_native_refill_rollback VALUES (1)")?;
        Ok(())
    });
    assert!(transaction.is_err());
    assert_eq!(seed(&connection)?, before);
    assert_eq!(connection.list_error_repair_links(WS, KEY)?, history);
    connection.insert_memory_seal(
        &first,
        &format!("blake3:{}", "a".repeat(64)),
        "2000-01-01T00:00:00Z",
    )?;
    assert!(!seed(&connection)?.contains(MEMORY_BODY));
    connection.mark_memory_seal_revealed(&first, "2000-01-01T00:00:00Z")?;
    assert_eq!(seed(&connection)?, before);
    connection.begin_read_snapshot()?;
    assert_eq!(seed(&connection)?, before);
    connection.commit_read_snapshot()?;
    Ok(())
}

#[test]
fn late_native_authority_failure_returns_no_partial_query_and_keeps_transaction_ownership()
-> TestResult {
    let connection = db()?;
    let good = memory(&connection, 1000, GOOD_BODY)?;
    let linked_rule = rule(&connection, 1000)?;
    let mut targets = (1..=96_u128)
        .map(|number| MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string())
        .collect::<Vec<_>>();
    targets.extend([good.clone(), linked_rule.clone()]);
    record(&connection, targets, Vec::new())?;
    let before = seed(&connection)?;
    assert!(before.contains(GOOD_BODY) && before.contains(RULE_BODY));
    let transaction: crate::db::Result<()> = connection.with_transaction(|| {
        connection.execute_raw(
            "ALTER TABLE rule_source_memories RENAME TO unavailable_late_native_authority",
        )?;
        let error = seed(&connection)
            .expect_err("selected memory cannot make unchecked rule authority optional");
        let message = error.to_string();
        for private in [
            GOOD_BODY,
            RULE_BODY,
            good.as_str(),
            linked_rule.as_str(),
            "unavailable_late_native_authority",
        ] {
            assert!(!message.contains(private));
        }
        // The reader neither commits nor rolls back the caller's schema edit.
        assert!(
            connection
                .query(
                    "SELECT memory_id FROM unavailable_late_native_authority",
                    &[]
                )
                .is_ok()
        );
        connection.execute_raw("INSERT INTO missing_late_native_rollback VALUES (1)")?;
        Ok(())
    });
    assert!(transaction.is_err());
    assert_eq!(seed(&connection)?, before);
    connection.begin_read_snapshot()?;
    assert_eq!(seed(&connection)?, before);
    connection.commit_read_snapshot()?;
    Ok(())
}
