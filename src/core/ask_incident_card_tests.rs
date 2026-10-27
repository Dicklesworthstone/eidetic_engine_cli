//! Ordinary answer admission must not treat an old card as permanent proof.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::incident_card::proof::tests::{Fixture, TestResult, WS};
use crate::models::MemoryScope;
use std::collections::BTreeMap;

fn corpus(
    fixture: &Fixture,
    scope: MemoryScope,
) -> TestResult<(Vec<AskCandidate>, BTreeMap<String, String>)> {
    fixture.db.begin_read_snapshot()?;
    let mut candidates = Vec::new();
    let mut sources = BTreeMap::new();
    let result = append_evidence(&fixture.db, WS, scope, &mut candidates, &mut sources);
    fixture.db.rollback_read_snapshot()?;
    result.map_err(|error| format!("{error:?}"))?;
    Ok((
        candidates,
        sources
            .into_iter()
            .map(|(id, source)| (id, source.entity_revision))
            .collect(),
    ))
}

#[test]
fn ask_withholds_historical_card_without_hiding_the_original_admitted_conversation() -> TestResult {
    let fixture = Fixture::new(false)?;
    let id = fixture.old_card()?;
    let card = fixture.db.get_evidence_span(&id)?.ok_or("card")?;
    let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
    assert!(card.is_direct_pack_admitted_for_session(WS, &session));
    assert!(public_evidence_body(&card.reader_text()));
    for scope in [MemoryScope::Workspace, MemoryScope::Swarm] {
        let (candidates, sources) = corpus(&fixture, scope)?;
        assert!(!candidates.iter().any(|candidate| candidate.memory_id == id));
        assert!(!sources.contains_key(&id));
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.memory_id == fixture.repair)
        );
        assert!(sources.contains_key(&fixture.repair));
    }
    assert!(fixture.db.get_evidence_span(&id)?.is_some());
    Ok(())
}

#[test]
fn ask_keeps_current_card_text_citation_revision_and_neutral_evidence_trust() -> TestResult {
    let fixture = Fixture::new(true)?;
    let card = fixture.current_card()?;
    let before = fixture
        .db
        .list_evidence_spans_for_session(&fixture.session)?;
    let (candidates, sources) = corpus(&fixture, MemoryScope::Workspace)?;
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.memory_id == card.id)
        .ok_or("current card must remain available")?;
    assert_eq!(candidate.content, card.reader_text().as_ref());
    assert_eq!(
        candidate.provenance_uri.as_deref(),
        Some(card.canonical_provenance_uri().as_str())
    );
    assert_eq!(sources.get(&card.id), Some(&card.pack_entity_revision()));
    assert_eq!(candidate.confidence, 0.5);
    assert_eq!(candidate.kind, "evidence");
    assert_eq!(candidate.trust_class, TrustClass::CassEvidence.as_str());
    for original in before {
        let after = fixture
            .db
            .get_evidence_span(&original.id)?
            .ok_or("source missing")?;
        assert_eq!(after.excerpt, original.excerpt);
        assert_eq!(after.content_hash, original.content_hash);
        assert_eq!(after.metadata_json, original.metadata_json);
    }
    let (again, revisions) = corpus(&fixture, MemoryScope::Workspace)?;
    assert_eq!(revisions, sources);
    assert_eq!(
        again
            .iter()
            .map(|item| (&item.memory_id, &item.content))
            .collect::<Vec<_>>(),
        candidates
            .iter()
            .map(|item| (&item.memory_id, &item.content))
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn ask_does_not_require_failure_proof_for_ordinary_imported_turns() -> TestResult {
    let fixture = Fixture::new(false)?;
    let (candidates, sources) = corpus(&fixture, MemoryScope::Workspace)?;
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.memory_id == fixture.repair)
    );
    assert!(sources.contains_key(&fixture.repair));
    assert!(
        !candidates
            .iter()
            .any(|candidate| candidate.memory_id == fixture.proof)
    );
    Ok(())
}

#[test]
fn answer_card_checks_use_caller_snapshot_without_committing_its_mutations() -> TestResult {
    let fixture = Fixture::new(true)?;
    let card = fixture.current_card()?;
    let transaction: crate::db::Result<()> = fixture.db.with_transaction(|| {
        fixture.db.execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = 'unverifiable failure source' WHERE id = '{}'",
            fixture.failure
        ))?;
        let mut candidates = Vec::new();
        let mut sources = BTreeMap::new();
        append_evidence(
            &fixture.db,
            WS,
            MemoryScope::Workspace,
            &mut candidates,
            &mut sources,
        )
        .expect("query works but the card proof is refused");
        assert!(!candidates.iter().any(|item| item.memory_id == card.id));
        assert!(!sources.contains_key(&card.id));
        fixture
            .db
            .execute_raw("INSERT INTO unavailable_card_test_rollback VALUES (1)")?;
        Ok(())
    });
    assert!(transaction.is_err());
    let (candidates, sources) = corpus(&fixture, MemoryScope::Workspace)?;
    assert!(candidates.iter().any(|item| item.memory_id == card.id));
    assert!(sources.contains_key(&card.id));
    Ok(())
}

fn existing_candidate() -> AskCandidate {
    AskCandidate {
        memory_id: crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(999)).to_string(),
        content: "Existing release guidance remains independent.".to_owned(),
        confidence: 0.9,
        trust_class: "human_explicit".to_owned(),
        provenance_uri: None,
        level: "semantic".to_owned(),
        kind: "note".to_owned(),
        team_provenance: None,
    }
}

#[test]
fn refusing_a_card_preserves_preexisting_candidates_and_native_citation_consistency() -> TestResult
{
    let fixture = Fixture::new(false)?;
    let card = fixture.old_card()?;
    let mut candidates = vec![existing_candidate()];
    let id = candidates[0].memory_id.clone();
    let mut sources = BTreeMap::new();
    fixture.db.begin_read_snapshot()?;
    append_evidence(
        &fixture.db,
        WS,
        MemoryScope::Workspace,
        &mut candidates,
        &mut sources,
    )
    .map_err(|error| format!("{error:?}"))?;
    fixture.db.rollback_read_snapshot()?;
    assert_eq!(candidates[0].memory_id, id);
    assert!(!sources.contains_key(&card));
    assert!(
        !candidates
            .iter()
            .any(|candidate| candidate.memory_id == card)
    );
    assert!(sources.keys().all(|id| {
        candidates
            .iter()
            .any(|candidate| &candidate.memory_id == id)
    }));
    Ok(())
}

#[test]
fn storage_failure_cannot_leave_partially_appended_answer_candidates() -> TestResult {
    let fixture = Fixture::new(true)?;
    fixture.current_card()?;
    fixture
        .db
        .execute_raw("ALTER TABLE evidence_spans RENAME TO unavailable_card_answer_sources")?;
    let mut candidates = vec![existing_candidate()];
    let id = candidates[0].memory_id.clone();
    let mut sources = BTreeMap::new();
    let error = append_evidence(
        &fixture.db,
        WS,
        MemoryScope::Workspace,
        &mut candidates,
        &mut sources,
    )
    .expect_err("source authority is unavailable");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].memory_id, id);
    assert!(sources.is_empty());
    assert!(!format!("{error:?}").contains("unavailable_card_answer_sources"));
    fixture.db.begin_read_snapshot()?;
    fixture.db.rollback_read_snapshot()?;
    Ok(())
}

#[test]
fn harmful_incident_guidance_is_not_an_answer_candidate_even_after_helpful_reconfirmation()
-> TestResult {
    use crate::core::incident_card::proof::tests::record_card_feedback;
    for target_is_card in [false, true] {
        let fixture = Fixture::new(true)?;
        let card = fixture.current_card()?;
        let (before, _) = corpus(&fixture, MemoryScope::Workspace)?;
        assert!(
            before
                .iter()
                .any(|candidate| candidate.memory_id == card.id)
        );
        let target = if target_is_card {
            &card.id
        } else {
            &fixture.repair
        };
        record_card_feedback(&fixture.db, WS, &fixture.diagnostic, target, true)?;
        record_card_feedback(&fixture.db, WS, &fixture.diagnostic, target, false)?;
        let history = fixture
            .db
            .list_error_repair_links(WS, &fixture.diagnostic.layered_key().key)?;
        for scope in [MemoryScope::Workspace, MemoryScope::Swarm] {
            let (candidates, sources) = corpus(&fixture, scope)?;
            assert!(
                !candidates
                    .iter()
                    .any(|candidate| candidate.memory_id == card.id)
            );
            assert!(!sources.contains_key(&card.id));
            assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.memory_id == fixture.repair)
            );
            assert!(sources.contains_key(&fixture.repair));
        }
        let stored = fixture
            .db
            .get_evidence_span(&card.id)?
            .ok_or("stored historical card")?;
        assert_eq!(stored.excerpt, card.excerpt);
        assert_eq!(stored.content_hash, card.content_hash);
        assert_eq!(
            stored.canonical_provenance_uri(),
            card.canonical_provenance_uri()
        );
        assert_eq!(
            fixture
                .db
                .list_error_repair_links(WS, &fixture.diagnostic.layered_key().key)?,
            history
        );
    }
    Ok(())
}

#[test]
fn answer_feedback_failure_never_leaves_partial_card_text_or_native_citations() -> TestResult {
    let fixture = Fixture::new(true)?;
    fixture.current_card()?;
    fixture
        .db
        .execute_raw("ALTER TABLE error_repair_links RENAME TO unavailable_answer_card_feedback")?;
    let mut candidates = vec![existing_candidate()];
    let existing_id = candidates[0].memory_id.clone();
    let mut sources = BTreeMap::new();
    fixture.db.begin_read_snapshot()?;
    let error = append_evidence(
        &fixture.db,
        WS,
        MemoryScope::Workspace,
        &mut candidates,
        &mut sources,
    )
    .expect_err("feedback authority is unavailable");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].memory_id, existing_id);
    assert!(sources.is_empty());
    assert!(!format!("{error:?}").contains("unavailable_answer_card_feedback"));
    fixture.db.commit_read_snapshot()?;
    Ok(())
}

#[test]
fn public_error_pack_withholds_a_card_when_its_source_repair_receives_harmful_feedback()
-> TestResult {
    use crate::core::error_diagnosis::{
        error_recall_report, pack_error_recall_query_seed, recalled_repair_evidence,
    };
    use crate::core::incident_card::proof::tests::record_card_feedback;
    let fixture = Fixture::new_public(true)?;
    let card = fixture.current_card()?;
    // A plain rustc line is imported with rustc's native code, even when the
    // shell invocation that emitted it was cargo check.
    let diagnostic = crate::core::error_recall::from_rustc(Some("E0308"), "mismatched types");
    let before = pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?;
    assert!(before.contains(&card.id));
    let report = error_recall_report(&fixture.db, WS, &diagnostic)?;
    assert!(report.helpful_repairs.contains(&card.id));
    record_card_feedback(&fixture.db, WS, &diagnostic, &fixture.repair, true)?;
    let after = pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?;
    assert!(!after.contains(&card.id));
    assert!(!after.contains(&fixture.repair));
    let evidence = recalled_repair_evidence(&fixture.db, WS, &report)?;
    assert!(!evidence.iter().any(|entry| entry.evidence_id == card.id));
    assert!(
        !evidence
            .iter()
            .any(|entry| entry.evidence_id == fixture.repair)
    );
    assert!(
        evidence
            .iter()
            .any(|entry| entry.evidence_id == fixture.proof
                && entry.role == "proof"
                && entry.text.is_none())
    );
    assert_eq!(
        fixture
            .db
            .get_evidence_span(&card.id)?
            .ok_or("historical card")?
            .excerpt,
        card.excerpt
    );
    Ok(())
}
