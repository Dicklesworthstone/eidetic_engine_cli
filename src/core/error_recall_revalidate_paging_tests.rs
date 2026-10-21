//! Rejected claims cannot exhaust the positive target window.

use super::super::super::{pack_targets, pack_targets_with};
use super::*;

fn record(fixture: &Fixture, targets: Vec<String>, actor: Option<&str>) -> TestResult {
    record_error_repair_links(
        &fixture.db,
        WS,
        &fixture.diagnostic,
        &ErrorRepairLinkRecording {
            helpful_repairs: targets,
            created_by: actor.map(str::to_owned),
            evidence_ref: Some(fixture.failure.clone()),
            ..ErrorRepairLinkRecording::default()
        },
    )?;
    Ok(())
}

fn id(number: u128) -> String {
    EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string()
}

fn manual_sources(fixture: &Fixture, count: u32) -> TestResult<Vec<String>> {
    let mut ids = Vec::new();
    for index in 0..count {
        let target = id(100_000 + u128::from(index));
        let excerpt = json!({"type":"assistant","message":{"role":"assistant","content":[
            {"type":"text","text":format!("MANUAL_FIX_{index} uses the canonical cache key for reads and writes.")}
        ]}}).to_string();
        fixture.db.insert_evidence_span(
            &target,
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: fixture.session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("manual:{index}"),
                span_kind: "message".to_owned(),
                start_line: 100 + index,
                end_line: 100 + index,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        ids.push(target);
    }
    record(fixture, ids.clone(), Some("human review"))?;
    Ok(ids)
}

#[test]
fn refills_after_96_unverified_claims_and_reuses_the_source_session() -> TestResult {
    let fixture = Fixture::new(true)?;
    fixture.historical(Some(CASS_ERROR_RECALL_ACTOR), &fixture.failure)?;
    let bad: Vec<_> = (1..=96).map(id).collect();
    record(&fixture, bad.clone(), Some(CASS_ERROR_RECALL_ACTOR))?;
    let good = manual_sources(&fixture, 36)?;
    assert!(bad.last().ok_or("bad prefix")? < &fixture.repair);
    assert!(&fixture.repair < good.first().ok_or("good suffix")?);
    let key = fixture.diagnostic.layered_key().key;
    assert_eq!(
        fixture
            .db
            .list_helpful_error_repair_target_ids(WS, &key, 32)?,
        bad[..32]
    );
    fixture.db.begin_read_snapshot()?;
    let mut gate = Gate::new(&fixture.db, WS, &key);
    let mut sizes = Vec::new();
    let selected = pack_targets_with(&fixture.db, WS, &key, 32, |ids| {
        sizes.push(ids.len());
        gate.withheld(ids)
    })?;
    let expected: Vec<_> = std::iter::once(fixture.repair.clone())
        .chain(good[..31].iter().cloned())
        .collect();
    assert_eq!(selected, expected);
    assert_eq!(sizes, [32, 32, 32, 32]);
    assert_eq!(
        gate.source_reads, 1,
        "do not reinterpret one source on every page"
    );
    fixture.db.rollback_read_snapshot()?;
    let query = pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)?;
    assert!(query.contains("prior fix:"));
    assert!(query.contains("flushing pending writes"));
    assert_eq!(query.matches("\nprior fix: ").count(), 4);
    assert!(query.len() <= 4096);
    assert_eq!(pack_targets(&fixture.db, WS, &key, 32)?, expected);
    Ok(())
}

#[test]
fn all_supported_limits_skip_invalid_prefixes_without_changing_history() -> TestResult {
    let fixture = Fixture::new(true)?;
    let bad: Vec<_> = (1..=41).map(id).collect();
    record(&fixture, bad, Some(CASS_ERROR_RECALL_ACTOR))?;
    let good = manual_sources(&fixture, 35)?;
    let history = fixture.history()?;
    let key = fixture.diagnostic.layered_key().key;
    for limit in 0..=32 {
        assert_eq!(
            pack_targets(&fixture.db, WS, &key, limit)?,
            good[..limit as usize]
        );
    }
    assert!(pack_targets(&fixture.db, WS, &key, 33).is_err());
    assert_eq!(fixture.history()?, history);
    Ok(())
}

#[test]
fn uncontested_projection_keeps_oversized_sentinels_and_exact_nul_aliases() -> TestResult {
    let fixture = Fixture::new(true)?;
    let good = manual_sources(&fixture, 35)?;
    record(
        &fixture,
        vec![
            format!("!{}", "🦀".repeat(4096)),
            format!("{}\0alias", good[0]),
        ],
        None,
    )?;
    let key = fixture.diagnostic.layered_key().key;
    let complete = fixture.history()?;
    for limit in 0..=32 {
        let legacy = fixture
            .db
            .list_helpful_error_repair_target_ids(WS, &key, limit)?;
        let selected = pack_targets(&fixture.db, WS, &key, limit)?;
        assert_eq!(selected, good[..limit as usize]);
        if limit > 0 {
            assert!(legacy.first().is_some_and(String::is_empty));
        }
        if limit > 2 {
            assert!(legacy.contains(&format!("{}\0alias", good[0])));
        }
    }
    assert_eq!(fixture.history()?, complete);
    Ok(())
}

#[test]
fn server_side_ordering_cursor_advances_past_multiple_oversized_projection_keys() -> TestResult {
    let fixture = Fixture::new(true)?;
    let good = manual_sources(&fixture, 4)?;
    record(
        &fixture,
        vec![
            format!("!{}", "x".repeat(16_384)),
            format!("!{}", "y".repeat(16_384)),
        ],
        None,
    )?;
    let key = fixture.diagnostic.layered_key().key;
    let mut calls = 0;
    let selected = pack_targets_with(&fixture.db, WS, &key, 2, |ids| {
        calls += 1;
        Ok(ids
            .iter()
            .filter(|id| id.is_empty())
            .map(|id| (*id).to_owned())
            .collect())
    })?;
    assert_eq!(calls, 2);
    assert_eq!(selected, good[..2]);
    Ok(())
}

#[test]
fn later_gate_errors_return_no_partial_window_and_leave_the_callers_snapshot_owned() -> TestResult {
    let fixture = Fixture::new(true)?;
    manual_sources(&fixture, 8)?;
    let key = fixture.diagnostic.layered_key().key;
    fixture.db.begin_read_snapshot()?;
    let mut calls = 0;
    let selected = pack_targets_with(&fixture.db, WS, &key, 3, |ids| {
        calls += 1;
        if calls == 2 {
            return Err(unavailable());
        }
        Ok(ids.iter().skip(1).map(|id| (*id).to_owned()).collect())
    });
    assert!(selected.is_err());
    assert_eq!(calls, 2);
    fixture.db.rollback_read_snapshot()?;
    fixture.db.begin_read_snapshot()?;
    fixture.db.rollback_read_snapshot()?;
    Ok(())
}

#[test]
fn source_storage_failure_is_not_downgraded_to_an_empty_valid_claim_set() -> TestResult {
    let fixture = Fixture::new(true)?;
    fixture.historical(Some(CASS_ERROR_RECALL_ACTOR), &fixture.failure)?;
    fixture
        .db
        .execute_raw("ALTER TABLE sessions RENAME TO private_missing_session_authority")?;
    let error = pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)
        .expect_err("source authority unavailable");
    assert!(
        !error
            .to_string()
            .contains("private_missing_session_authority")
    );
    fixture.db.begin_read_snapshot()?;
    fixture.db.rollback_read_snapshot()?;
    Ok(())
}
