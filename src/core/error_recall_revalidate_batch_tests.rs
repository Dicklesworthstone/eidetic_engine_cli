//! Bounded owner reads and session-grouped validation over real migrated rows.

use super::*;

struct Episode {
    session: String,
    failure: String,
    repair: String,
    card: String,
}

fn evidence_id(number: u128) -> String {
    EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string()
}

fn append_episode(fixture: &Fixture, number: u32) -> TestResult<Episode> {
    let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(
        0x80_0000 + u128::from(number),
    )).to_string();
    fixture.db.insert_session(&session, &CreateSessionInput {
        workspace_id: WS.to_owned(), cass_session_id: format!("/sessions/batch-{number}.jsonl"),
        source_path: None, agent_name: Some("claude_code".to_owned()), model: None,
        started_at: None, ended_at: None, message_count: 5, token_count: None,
        content_hash: format!("blake3:{}", blake3::hash(&number.to_le_bytes()).to_hex()),
        metadata_json: None,
    })?;
    let failure = go(&[("TestCache", "fail"), ("TestControl", "pass")]);
    let success = go(&[("TestCache", "pass"), ("TestControl", "pass"), ("TestRegression", "pass")]);
    let rows = [
        ("tool_call", "assistant", json!({"type":"tool_use","id":"fail","name":"Bash","input":{"command":COMMAND}})),
        ("tool_result", "user", json!({"type":"tool_result","tool_use_id":"fail","content":failure,"is_error":true})),
        ("message", "assistant", json!({"type":"text","text":REPAIR})),
        ("tool_call", "assistant", json!({"type":"tool_use","id":"retry","name":"Bash","input":{"command":COMMAND}})),
        ("tool_result", "user", json!({"type":"tool_result","tool_use_id":"retry","content":success,"is_error":false})),
    ];
    let id = |line: u32| evidence_id(0x90_0000 + u128::from(number) * 16 + u128::from(line));
    for (offset, (kind, role, block)) in rows.into_iter().enumerate() {
        let line = u32::try_from(offset + 1)?;
        let excerpt = json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
        fixture.db.insert_evidence_span(&id(line), &CreateEvidenceSpanInput {
            workspace_id: WS.to_owned(), session_id: session.clone(), memory_id: None,
            producer_kind: EvidenceProducerKind::CassImport, cass_span_id: format!("{session}:{line}"),
            span_kind: kind.to_owned(), start_line: line, end_line: line,
            start_byte: None, end_byte: None, role: Some(role.to_owned()),
            content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
            excerpt, metadata_json: None, inherited_redaction_classes: Vec::new(),
        })?;
    }
    let report = record_session_error_recall(&fixture.db, WS, &session)?;
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.incident_cards_recorded, 1);
    Ok(Episode {
        session, failure: id(2), repair: id(3),
        card: crate::core::incident_card::incident_card_id(WS, &id(2)),
    })
}

fn claim(fixture: &Fixture, target: &str, failure: &str) -> TestResult {
    record_error_repair_links(&fixture.db, WS, &fixture.diagnostic, &ErrorRepairLinkRecording {
        helpful_repairs: vec![target.to_owned()], created_by: Some(CASS_ERROR_RECALL_ACTOR.to_owned()),
        evidence_ref: Some(failure.to_owned()), ..ErrorRepairLinkRecording::default()
    })?;
    Ok(())
}

fn interleaved(fixture: &Fixture) -> TestResult<(Vec<String>, BTreeSet<String>)> {
    let episodes = (0..5).map(|number| append_episode(fixture, number))
        .collect::<TestResult<Vec<_>>>()?;
    let mut targets = Vec::new();
    let mut rejected = BTreeSet::new();
    for round in 0..20_u32 {
        for (index, episode) in episodes.iter().enumerate() {
            // Sorted target identities cycle through five owners, exceeding
            // the four-entry cache. These claimed targets have no real arc.
            let target = evidence_id(1 + u128::from(round) * 5 + u128::try_from(index)?);
            claim(fixture, &target, &episode.failure)?;
            rejected.insert(target.clone());
            targets.push(target);
        }
    }
    for episode in episodes {
        targets.extend([episode.repair, episode.card]);
    }
    assert_eq!(targets.len(), 110);
    Ok((targets, rejected))
}

#[test]
fn interleaved_targets_resolve_owners_once_and_interpret_each_session_once() -> TestResult {
    let fixture = Fixture::new(true)?;
    let (targets, expected) = interleaved(&fixture)?;
    let key = fixture.diagnostic.layered_key().key;
    let refs: Vec<_> = targets.iter().map(String::as_str).collect();
    let history = fixture.history()?;
    fixture.db.begin_read_snapshot()?;
    let mut gate = Gate::new(&fixture.db, WS, &key);
    assert_eq!(gate.withheld(&refs)?, expected);
    assert_eq!(gate.ownership_reads, 1, "one bounded owner query, not 110 point reads");
    assert_eq!(gate.source_reads, 5, "one interpretation per source, not per target");
    assert_eq!(gate.sessions.len(), MAX_CACHED_SESSIONS);
    fixture.db.rollback_read_snapshot()?;
    assert_eq!(fixture.history()?, history);
    Ok(())
}

#[test]
fn all_cached_groups_are_used_before_missing_groups_can_evict_them() -> TestResult {
    let fixture = Fixture::new(true)?;
    let (mut targets, expected) = interleaved(&fixture)?;
    let key = fixture.diagnostic.layered_key().key;
    fixture.db.begin_read_snapshot()?;
    let mut gate = Gate::new(&fixture.db, WS, &key);
    let refs: Vec<_> = targets.iter().map(String::as_str).collect();
    assert_eq!(gate.withheld(&refs)?, expected);
    assert_eq!(gate.source_reads, 5);
    targets.reverse();
    let refs: Vec<_> = targets.iter().map(String::as_str).collect();
    assert_eq!(gate.withheld(&refs)?, expected);
    assert_eq!(gate.source_reads, 6, "all four warm groups survive until inspected");
    assert_eq!(gate.ownership_reads, 2);
    assert_eq!(gate.sessions.len(), MAX_CACHED_SESSIONS);
    fixture.db.rollback_read_snapshot()?;
    Ok(())
}

#[test]
fn grouped_validation_preserves_cited_cards_pack_content_and_source_rows() -> TestResult {
    let fixture = Fixture::new(true)?;
    let (targets, rejected) = interleaved(&fixture)?;
    let history = fixture.history()?;
    let report = error_recall_report(&fixture.db, WS, &fixture.diagnostic)?;
    let presented = recalled_repair_evidence(&fixture.db, WS, &report)?;
    assert_eq!(presented.iter().filter(|item| item.role == "incident_card").count(), 5);
    assert_eq!(presented.iter().filter(|item| item.role == "repair").count(), 5);
    assert_eq!(presented.iter().filter(|item| item.role == "proof").count(), 5);
    for item in &presented {
        assert!(!rejected.contains(&item.evidence_id));
        let source = fixture.db.get_evidence_span(&item.evidence_id)?.ok_or("source missing")?;
        assert_eq!(item.provenance_uri, source.canonical_provenance_uri());
        assert_eq!(item.text.is_some(), item.role != "proof");
    }
    let before = targets.iter().filter(|id| !rejected.contains(*id))
        .map(|id| fixture.db.get_evidence_span(id))
        .collect::<crate::db::Result<Vec<_>>>()?;
    let seed = pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)?;
    assert!(seed.contains("prior fix:"));
    assert!(seed.contains("flushing pending writes"));
    assert_eq!(seed.matches("\nprior fix: ").count(), 4);
    assert!(seed.len() <= 4096);
    for original in before.into_iter().flatten() {
        let current = fixture.db.get_evidence_span(&original.id)?.ok_or("source missing")?;
        assert_eq!(current.excerpt, original.excerpt);
        assert_eq!(current.content_hash, original.content_hash);
        assert_eq!(current.metadata_json, original.metadata_json);
        assert_eq!(current.canonical_provenance_uri(), original.canonical_provenance_uri());
    }
    assert_eq!(fixture.history()?, history);
    Ok(())
}

#[test]
fn owner_batch_preserves_exact_scope_and_refuses_missing_foreign_or_aliased_failures() -> TestResult {
    let fixture = Fixture::new(true)?;
    fixture.historical(Some(CASS_ERROR_RECALL_ACTOR), &fixture.failure)?;
    let other = append_episode(&fixture, 10)?;
    fixture.db.execute_raw(&format!(
        "UPDATE evidence_spans SET workspace_id = '{OTHER}' WHERE id = '{}'", other.failure
    ))?;
    let foreign = evidence_id(101);
    let missing = evidence_id(102);
    let alias = evidence_id(103);
    claim(&fixture, &foreign, &other.failure)?;
    claim(&fixture, &missing, &evidence_id(0xdead_beef))?;
    claim(&fixture, &alias, &format!("{}\0alias", fixture.failure))?;
    let key = fixture.diagnostic.layered_key().key;
    let mut gate = Gate::new(&fixture.db, WS, &key);
    let targets = [fixture.repair.as_str(), foreign.as_str(), missing.as_str(), alias.as_str()];
    let rejected = gate.withheld(&targets)?;
    assert_eq!(rejected, BTreeSet::from([foreign, missing, alias]));
    assert_eq!(gate.ownership_reads, 1);
    assert_eq!(gate.source_reads, 1, "missing/foreign owners never hydrate sessions");
    let malformed = BTreeSet::from(["not_an_evidence_id"]);
    assert!(gate.failure_owners(&malformed).is_err());
    let too_many: Vec<_> = (0..=128).map(|n| evidence_id(1 + n)).collect();
    assert!(gate.failure_owners(&too_many.iter().map(String::as_str).collect()).is_err());
    assert_eq!(gate.ownership_reads, 1);
    Ok(())
}

#[test]
fn manual_harmful_and_empty_batches_do_not_load_source_ownership() -> TestResult {
    let fixture = Fixture::new(true)?;
    fixture.historical(Some("human review"), &fixture.failure)?;
    let key = fixture.diagnostic.layered_key().key;
    let mut gate = Gate::new(&fixture.db, WS, &key);
    assert!(gate.withheld(&[])?.is_empty());
    assert!(gate.withheld(&[&fixture.repair])?.is_empty());
    assert_eq!(gate.ownership_reads, 0);
    fixture.historical(Some(CASS_ERROR_RECALL_ACTOR), &fixture.failure)?;
    record_error_repair_links(&fixture.db, WS, &fixture.diagnostic, &ErrorRepairLinkRecording {
        harmful_repairs: vec![fixture.repair.clone()], ..ErrorRepairLinkRecording::default()
    })?;
    assert_eq!(gate.withheld(&[&fixture.repair])?, BTreeSet::from([fixture.repair.clone()]));
    assert_eq!(gate.ownership_reads, 0);
    assert_eq!(gate.source_reads, 0);
    assert!(gate.sessions.is_empty());
    Ok(())
}

#[test]
fn grouped_gate_preserves_caller_rollback_and_propagates_unavailable_ownership() -> TestResult {
    let fixture = Fixture::new(true)?;
    let episode = append_episode(&fixture, 11)?;
    let key = fixture.diagnostic.layered_key().key;
    let targets = [episode.repair.as_str(), episode.card.as_str()];
    let before = fixture.db.get_session(&episode.session)?.ok_or("session")?;
    let result: crate::db::Result<()> = fixture.db.with_transaction(|| {
        fixture.db.execute_raw(&format!(
            "UPDATE sessions SET workspace_id = '{OTHER}' WHERE id = '{}'", episode.session
        ))?;
        let mut gate = Gate::new(&fixture.db, WS, &key);
        assert_eq!(gate.withheld(&targets)?.len(), 2);
        assert_eq!(gate.ownership_reads, 1);
        assert_eq!(gate.source_reads, 1);
        Err(unavailable())
    });
    assert!(result.is_err());
    let after = fixture.db.get_session(&episode.session)?.ok_or("session")?;
    assert_eq!(after.workspace_id, before.workspace_id);
    let mut gate = Gate::new(&fixture.db, WS, &key);
    assert!(gate.withheld(&targets)?.is_empty());
    fixture.db.execute_raw("ALTER TABLE evidence_spans RENAME TO private_unavailable_batch_source")?;
    let error = gate.withheld(&targets).expect_err("cached verdict cannot bypass missing owner authority");
    assert!(!error.to_string().contains("private_unavailable_batch_source"));
    fixture.db.begin_read_snapshot()?;
    fixture.db.rollback_read_snapshot()?;
    Ok(())
}

#[test]
fn full_owner_batch_uses_all_128_parameters_and_rejects_overflow_before_io() -> TestResult {
    let fixture = Fixture::new(true)?;
    let key = fixture.diagnostic.layered_key().key;
    let mut ids: Vec<_> = (0..127).map(|number| evidence_id(0xdead_0000 + number)).collect();
    ids.push(fixture.failure.clone());
    let mut gate = Gate::new(&fixture.db, WS, &key);
    let selected = ids.iter().map(String::as_str).collect();
    assert_eq!(gate.failure_owners(&selected)?, BTreeMap::from([
        (fixture.failure.clone(), fixture.session.clone())
    ]));
    assert_eq!(gate.ownership_reads, 1);
    ids.push(evidence_id(0xdead_1000));
    assert!(gate.failure_owners(&ids.iter().map(String::as_str).collect()).is_err());
    assert_eq!(gate.ownership_reads, 1);
    assert_eq!(gate.source_reads, 0);
    Ok(())
}
