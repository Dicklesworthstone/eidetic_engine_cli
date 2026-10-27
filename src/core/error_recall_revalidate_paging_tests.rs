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

fn raw_cursor_page(
    fixture: &Fixture,
    after: Option<&str>,
    limit: i64,
) -> TestResult<Vec<(String, String)>> {
    let key = fixture.diagnostic.layered_key().key;
    let mut parameters = vec![
        Value::Text(WS.to_owned()),
        Value::Text(key),
        Value::BigInt(limit),
    ];
    if let Some(after) = after {
        parameters.push(Value::Text(after.to_owned()));
    }
    let sql = super::super::super::pack_target_page_sql(after.is_some());
    let mut page = Vec::new();
    for row in fixture.db.query(&sql, &parameters)? {
        page.push((
            row.get(0)
                .and_then(Value::as_str)
                .ok_or("target")?
                .to_owned(),
            row.get(1)
                .and_then(Value::as_str)
                .ok_or("cursor")?
                .to_owned(),
        ));
    }
    Ok(page)
}

#[test]
fn seek_pages_preserve_raw_target_order_projection_and_negative_feedback() -> TestResult {
    let fixture = Fixture::new(true)?;
    let good = manual_sources(&fixture, 36)?;
    let mut unusual = Vec::new();
    for number in 0..129 {
        unusual.push(match number % 3 {
            0 => format!("!{number:04}-{}", "🦀".repeat(100)),
            1 => format!("{}\0alias-{number:04}", good[0]),
            _ => format!("界-{number:04}"),
        });
    }
    record(&fixture, unusual.clone(), None)?;
    let harmful: BTreeSet<_> = unusual.iter().step_by(5).cloned().collect();
    record_error_repair_links(
        &fixture.db,
        WS,
        &fixture.diagnostic,
        &ErrorRepairLinkRecording {
            harmful_repairs: harmful.iter().cloned().collect(),
            ..Default::default()
        },
    )?;
    let key = fixture.diagnostic.layered_key().key;
    let history = fixture.db.list_error_repair_links(WS, &key)?;
    let mut expected: Vec<_> = history
        .iter()
        .filter(|link| {
            link.link_kind == "repair"
                && link.outcome == "helpful"
                && !harmful.contains(&link.target_id)
        })
        .collect();
    expected.sort_by(|left, right| {
        left.target_id
            .cmp(&right.target_id)
            .then_with(|| left.link_id.cmp(&right.link_id))
    });
    let expected: Vec<_> = expected
        .into_iter()
        .map(|link| {
            (
                if link.target_id.len() <= 128 {
                    link.target_id.clone()
                } else {
                    String::new()
                },
                link.link_id.clone(),
            )
        })
        .collect();
    assert!(expected.iter().any(|(target, _)| target.is_empty()));
    assert!(expected.iter().any(|(target, _)| target.contains('\0')));
    for limit in [1_i64, 2, 7, 31, 32] {
        let mut after: Option<String> = None;
        let mut actual = Vec::new();
        loop {
            let page = raw_cursor_page(&fixture, after.as_deref(), limit)?;
            if page.is_empty() {
                break;
            }
            assert!(page.len() <= limit as usize);
            assert!(
                page.iter()
                    .all(|(target, cursor)| target.len() <= 128 && cursor.len() <= 128)
            );
            after = page.last().map(|(_, cursor)| cursor.clone());
            actual.extend(page);
            assert!(
                actual.len() <= expected.len(),
                "a cursor revisited an earlier target"
            );
        }
        assert_eq!(actual, expected);
    }
    assert_eq!(fixture.db.list_error_repair_links(WS, &key)?, history);
    assert_eq!(pack_targets(&fixture.db, WS, &key, 32)?, good[..32]);
    Ok(())
}

#[test]
fn seek_cursor_is_bound_to_the_workspace_and_error_class_without_aliasing() -> TestResult {
    let fixture = Fixture::new(true)?;
    manual_sources(&fixture, 8)?;
    let first = raw_cursor_page(&fixture, None, 1)?;
    let cursor = first.first().ok_or("first page")?.1.clone();
    let next = raw_cursor_page(&fixture, Some(&cursor), 1)?;
    assert_eq!(next.len(), 1);
    assert!(first[0].0 < next[0].0);
    for invalid in [
        "unknown".to_owned(),
        format!("{cursor}\0alias"),
        format!(" {cursor}"),
    ] {
        assert!(raw_cursor_page(&fixture, Some(&invalid), 32)?.is_empty());
    }
    for (workspace, diagnostic) in [
        (OTHER, fixture.diagnostic.clone()),
        (
            WS,
            crate::core::error_recall::from_rustc(Some("E0308"), "mismatched types"),
        ),
    ] {
        let links = record_error_repair_links(
            &fixture.db,
            workspace,
            &diagnostic,
            &ErrorRepairLinkRecording {
                helpful_repairs: vec![id(1)],
                ..Default::default()
            },
        )?;
        let foreign = links.first().ok_or("foreign cursor")?;
        assert!(raw_cursor_page(&fixture, Some(&foreign.link_id), 32)?.is_empty());
    }
    assert_eq!(raw_cursor_page(&fixture, Some(&cursor), 1)?, next);
    Ok(())
}
