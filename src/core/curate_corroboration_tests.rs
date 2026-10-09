use super::*;
use crate::core::curate::corroboration;
use crate::db::ReviewCorroborationLimits;

const LESSON: &str = "Always run cargo test --locked after changing src/support.rs.";

fn source_session(
    fixture: &ReviewFixture,
    seed: u128,
    source_path: &str,
    records: &[String],
    metadata_padding: &str,
) -> Result<String, String> {
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let id = SessionId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
    let mut input = session_input(&fixture.workspace_id, &format!("corroboration-{seed}"));
    input.source_path = Some(source_path.to_owned());
    input.metadata_json = Some(serde_json::json!({"fixturePadding": metadata_padding}).to_string());
    connection
        .insert_session(&id, &input)
        .map_err(|error| error.to_string())?;
    for (index, text) in records.iter().enumerate() {
        let evidence = evidence_id(seed * 100 + index as u128);
        let mut input = evidence_span_input(
            &fixture.workspace_id,
            &id,
            None,
            &evidence,
            u32::try_from(index + 1).map_err(|error| error.to_string())? * 3,
            text,
        );
        if text.contains("\"type\":\"tool_result\"") {
            input.span_kind = "tool_result".to_owned();
            input.role = Some("tool".to_owned());
        }
        connection
            .insert_evidence_span(&evidence, &input)
            .map_err(|error| error.to_string())?;
    }
    connection.close().map_err(|error| error.to_string())?;
    Ok(id)
}

fn independent_session(fixture: &ReviewFixture, seed: u128) -> Result<String, String> {
    source_session(
        fixture,
        seed,
        &format!("/tmp/cass/corroboration-{seed}.jsonl"),
        &[
            format!("Observation from session {seed}."),
            r#"{"type":"tool_result","content":"A routine command completed."}"#.to_owned(),
            LESSON.to_owned(),
        ],
        "",
    )
}

fn propose(
    fixture: &ReviewFixture,
    session: &str,
    floor: f32,
    persist: bool,
) -> Result<ReviewSessionReport, String> {
    review_session_proposals(&ReviewSessionOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        session_id: Some(session),
        propose: true,
        dry_run: !persist,
        min_confidence: floor,
        limit: 1,
    })
    .map_err(|error| error.message())
}

fn stored_candidate(
    connection: &DbConnection,
    fixture: &ReviewFixture,
    id: &str,
) -> Result<StoredCurationCandidate, String> {
    connection
        .get_curation_candidate(&fixture.workspace_id, id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "missing proposal".to_owned())
}

fn proof(stored: &StoredCurationCandidate) -> serde_json::Value {
    let metadata: serde_json::Value = serde_json::from_str(
        stored
            .derivation_metadata_json
            .as_deref()
            .expect("derivation metadata"),
    )
    .expect("canonical metadata JSON");
    metadata
        .pointer("/producer/producerPayload/corroboration")
        .expect("recorded proof")
        .clone()
}

#[test]
fn single_session_repeats_and_imported_aliases_do_not_raise_confidence() -> TestResult {
    let fixture = review_session_fixture()?;
    let context = "Observation from one conversation.".to_owned();
    let anchor = source_session(
        &fixture,
        70_001,
        "/tmp/cass/shared-lineage.jsonl",
        &[context.clone(), LESSON.to_owned(), LESSON.to_owned()],
        "",
    )?;
    source_session(
        &fixture,
        70_002,
        "/tmp/cass/copied-conversation.jsonl",
        &[context.clone(), LESSON.to_owned()],
        "",
    )?;
    source_session(
        &fixture,
        70_003,
        "/tmp/cass/repeated-copy.jsonl",
        &[
            context,
            LESSON.to_owned(),
            LESSON.to_owned(),
            LESSON.to_owned(),
        ],
        "",
    )?;
    source_session(
        &fixture,
        70_004,
        "/tmp/cass/shared-lineage.jsonl",
        &[
            "A differently wrapped observation from the same source path.".to_owned(),
            LESSON.to_owned(),
        ],
        "",
    )?;
    assert_eq!(propose(&fixture, &anchor, 0.8, false)?.candidate_count, 0);
    let initial = propose(&fixture, &anchor, 0.0, true)?;
    assert_eq!(initial.candidate_count, 1);
    let candidate = &initial.candidates[0];
    assert_eq!(candidate.confidence, 0.6);
    assert_eq!(
        candidate.source_ids.len(),
        2,
        "both original repeated source identities are retained"
    );
    let replay = propose(&fixture, &anchor, 0.0, true)?;
    assert!(!replay.durable_mutation);
    assert_eq!(replay.candidates[0].candidate_id, candidate.candidate_id);
    assert_eq!(replay.candidates[0].confidence, 0.6);
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let stored = stored_candidate(&connection, &fixture, &candidate.candidate_id)?;
    assert_eq!(
        proof(&stored)["sessions"]
            .as_array()
            .expect("sessions")
            .len(),
        1
    );
    corroboration::validate_recorded(&connection, &stored).map_err(|error| error.message)?;
    Ok(())
}

#[test]
fn independent_support_survives_supporter_apply_and_transactional_retry() -> TestResult {
    let fixture = review_session_fixture()?;
    let sessions = [
        independent_session(&fixture, 71_001)?,
        independent_session(&fixture, 71_002)?,
        independent_session(&fixture, 71_003)?,
    ];
    let mut proposals = Vec::new();
    for session in &sessions {
        // A high floor and a limit of one must be applied after corroboration.
        let report = propose(&fixture, session, 0.8, true)?;
        assert_eq!(report.candidate_count, 1);
        assert_eq!(report.candidates[0].confidence, 0.8);
        assert_eq!(
            report.candidates[0].source_ids.len(),
            1,
            "corroboration does not take ownership of a supporter"
        );
        proposals.push(report.candidates[0].clone());
    }
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let before = stored_candidate(&connection, &fixture, &proposals[0].candidate_id)?;
    assert_eq!(
        proof(&before)["sessions"]
            .as_array()
            .expect("sessions")
            .len(),
        3
    );
    let source_session = connection
        .get_session(&sessions[0])
        .map_err(|error| error.to_string())?
        .expect("session");
    let barrier = connection
        .get_evidence_span(&evidence_id(71_001 * 100 + 1))
        .map_err(|error| error.to_string())?
        .expect("tool barrier");
    assert!(barrier.is_class_a_derivation_readable(&fixture.workspace_id, &source_session));
    assert!(!barrier.is_search_admitted_for_session(&fixture.workspace_id, &source_session));
    connection
        .insert_evidence_span(
            &evidence_id(71_099 * 100),
            &evidence_span_input(
                &fixture.workspace_id,
                &sessions[1],
                None,
                "later-unrelated-turn",
                30,
                "An unrelated observation arrived after the proposal.",
            ),
        )
        .map_err(|error| error.to_string())?;
    corroboration::validate_recorded(&connection, &before).map_err(|error| error.message)?;
    validate_arc(&fixture, &proposals[1].candidate_id)?;
    let supporter_memory = apply_arc(&fixture, &proposals[1].candidate_id, false)?
        .application
        .created_memory_id
        .expect("supporter applied");
    // Applying a supporter attaches its source. It does not retract the lesson.
    corroboration::validate_recorded(&connection, &before).map_err(|error| error.message)?;
    validate_arc(&fixture, &proposals[0].candidate_id)?;
    let memory_count = connection
        .list_memories(&fixture.workspace_id, None, true)
        .map_err(|error| error.to_string())?
        .len();
    let audit_count = connection
        .list_audit_entries(Some(&fixture.workspace_id), None)
        .map_err(|error| error.to_string())?
        .len();
    crate::core::curate::set_create_derived_apply_fail_phase(Some(
        "before_insert_search_index_job",
    ));
    let failed = apply_arc(&fixture, &proposals[0].candidate_id, false);
    crate::core::curate::set_create_derived_apply_fail_phase(None);
    assert!(
        failed.is_err(),
        "the injected failure must roll back memory, evidence attachment and audit"
    );
    assert_eq!(
        connection
            .list_memories(&fixture.workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .len(),
        memory_count
    );
    assert_eq!(
        connection
            .list_audit_entries(Some(&fixture.workspace_id), None)
            .map_err(|error| error.to_string())?
            .len(),
        audit_count
    );
    let applied = apply_arc(&fixture, &proposals[0].candidate_id, false)?;
    assert_eq!(
        applied.application.status, "applied",
        "{:?}",
        applied.application.errors
    );
    let memory_id = applied
        .application
        .created_memory_id
        .expect("anchor applied");
    let after = stored_candidate(&connection, &fixture, &proposals[0].candidate_id)?;
    assert_eq!(
        after.derivation_metadata_json,
        before.derivation_metadata_json
    );
    assert_eq!(
        after.derivation_source_refs_json,
        before.derivation_source_refs_json
    );
    assert_eq!(after.confidence, 0.8);
    assert_eq!(
        connection
            .get_memory(&memory_id)
            .map_err(|error| error.to_string())?
            .expect("memory")
            .confidence,
        0.8
    );
    for (candidate, owner) in [
        (&proposals[0], &memory_id),
        (&proposals[1], &supporter_memory),
    ] {
        let source = connection
            .get_evidence_span(&candidate.source_ids[0])
            .map_err(|error| error.to_string())?
            .expect("source");
        assert_eq!(source.memory_id.as_deref(), Some(owner.as_str()));
    }
    let audit_count = connection
        .list_audit_entries(Some(&fixture.workspace_id), None)
        .map_err(|error| error.to_string())?
        .len();
    let replay = apply_arc(&fixture, &proposals[0].candidate_id, false)?;
    assert_eq!(replay.application.status, "already_applied");
    assert!(!replay.durable_mutation);
    assert_eq!(
        connection
            .list_audit_entries(Some(&fixture.workspace_id), None)
            .map_err(|error| error.to_string())?
            .len(),
        audit_count
    );
    let audits = connection
        .list_audit_entries(Some(&fixture.workspace_id), None)
        .map_err(|error| error.to_string())?;
    let create = audits
        .iter()
        .find(|entry| {
            entry.action == audit_actions::MEMORY_CREATE
                && entry.target_id.as_deref() == Some(memory_id.as_str())
        })
        .expect("memory creation audit");
    let details: serde_json::Value =
        serde_json::from_str(create.details.as_deref().expect("audit details"))
            .map_err(|error| error.to_string())?;
    assert_eq!(
        details["producerPayload"]["corroboration"],
        proof(&before),
        "the durable creation audit preserves exactly the reviewed support snapshot"
    );
    Ok(())
}

#[test]
fn historical_single_session_proposals_keep_their_recorded_confidence_and_proof() -> TestResult {
    let fixture = review_session_fixture()?;
    let anchor = independent_session(&fixture, 72_001)?;
    let initial = propose(&fixture, &anchor, 0.0, true)?;
    let id = &initial.candidates[0].candidate_id;
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let before = stored_candidate(&connection, &fixture, id)?;
    let second = independent_session(&fixture, 72_002)?;
    independent_session(&fixture, 72_003)?;
    assert_eq!(
        propose(&fixture, &anchor, 0.8, true)?.candidate_count,
        0,
        "current observations do not silently raise a historical score above the floor"
    );
    let replay = propose(&fixture, &anchor, 0.0, true)?;
    assert!(!replay.durable_mutation);
    assert_eq!(replay.candidates[0].candidate_id, *id);
    assert_eq!(replay.candidates[0].confidence, 0.6);
    assert!(replay.candidates[0].reason.contains("Historical proposal"));
    let after = stored_candidate(&connection, &fixture, id)?;
    assert_eq!(after.confidence, before.confidence);
    assert_eq!(after.proposed_confidence, before.proposed_confidence);
    assert_eq!(after.source_id, before.source_id);
    assert_eq!(after.reason, before.reason);
    assert_eq!(
        after.derivation_metadata_json,
        before.derivation_metadata_json
    );
    let fresh = propose(&fixture, &second, 0.8, true)?;
    assert_eq!(fresh.candidate_count, 1);
    assert_eq!(fresh.candidates[0].confidence, 0.8);
    assert_eq!(
        proof(&stored_candidate(
            &connection,
            &fixture,
            &fresh.candidates[0].candidate_id
        )?)["sessions"]
            .as_array()
            .expect("sessions")
            .len(),
        3
    );
    Ok(())
}

#[test]
fn independent_arcs_keep_the_complete_pair_and_its_exact_proof_on_apply() -> TestResult {
    let fixture = review_session_fixture()?;
    let mut sessions = Vec::new();
    for seed in 72_101..=72_103 {
        sessions.push(source_session(
            &fixture,
            seed,
            &format!("/tmp/cass/arc-{seed}"),
            &[
                format!("Observation from session {seed}."),
                INLINE_ARC.to_owned(),
            ],
            "",
        )?);
    }
    let options = ReviewSessionOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        session_id: Some(&sessions[0]),
        propose: true,
        dry_run: false,
        min_confidence: 0.8,
        limit: 2,
    };
    let report = review_session_proposals(&options).map_err(|error| error.message())?;
    assert_eq!(
        report.candidate_count, 2,
        "the independent-session tier must be scored before the floor and pair limit"
    );
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let mut recorded = Vec::new();
    for candidate in &report.candidates {
        assert!(candidate.session_arc.is_some());
        assert_eq!(candidate.confidence, 0.8);
        assert_eq!(candidate.source_ids.len(), 1);
        let stored = stored_candidate(&connection, &fixture, &candidate.candidate_id)?;
        assert_eq!(
            proof(&stored)["sessions"]
                .as_array()
                .expect("sessions")
                .len(),
            3
        );
        recorded.push(stored);
    }
    for candidate in &report.candidates {
        validate_arc(&fixture, &candidate.candidate_id)?;
        let applied = apply_arc(&fixture, &candidate.candidate_id, false)?;
        assert_eq!(
            applied.application.status, "applied",
            "{:?}",
            applied.application.errors
        );
    }
    let replay = review_session_proposals(&options).map_err(|error| error.message())?;
    assert_eq!(replay.candidate_count, 2);
    assert!(!replay.durable_mutation);
    for before in &recorded {
        let after = stored_candidate(&connection, &fixture, &before.id)?;
        assert_eq!(
            after.derivation_metadata_json,
            before.derivation_metadata_json
        );
        assert_eq!(
            after.derivation_source_refs_json,
            before.derivation_source_refs_json
        );
        assert_eq!(after.confidence, 0.8);
        assert_eq!(
            apply_arc(&fixture, &before.id, false)?.application.status,
            "already_applied"
        );
    }
    Ok(())
}

#[test]
fn complete_source_qualifications_and_refuted_or_withheld_sessions_do_not_corroborate() -> TestResult
{
    let fixture = review_session_fixture()?;
    let common = format!("{LESSON}\nKeep src/support.rs stable across the release.");
    let anchor_text = format!(
        "{common}\n{} Only apply this to protocol 17.",
        "Operational qualification ".repeat(15)
    );
    let anchor = source_session(
        &fixture,
        73_001,
        "/tmp/cass/qualified-a",
        &["Observation alpha.".to_owned(), anchor_text.clone()],
        "",
    )?;
    let changed = source_session(
        &fixture,
        73_002,
        "/tmp/cass/qualified-b",
        &[
            "Observation beta.".to_owned(),
            anchor_text.replace("protocol 17", "protocol 18"),
        ],
        "",
    )?;
    source_session(
        &fixture,
        73_003,
        "/tmp/cass/qualified-c",
        &[
            "Observation gamma.".to_owned(),
            anchor_text.replace("Only apply", "Never apply"),
        ],
        "",
    )?;
    let rejected = source_session(
        &fixture,
        73_004,
        "/tmp/cass/refuted",
        &[
            "Observation delta.".to_owned(),
            anchor_text.clone(),
            "This advice is wrong; do not follow it.".to_owned(),
        ],
        "",
    )?;
    source_session(
        &fixture,
        73_005,
        "/tmp/cass/withheld",
        &[
            "Observation epsilon.".to_owned(),
            anchor_text.clone(),
            r#"{"type":"assistant","content":"\u12zz"}"#.to_owned(),
        ],
        "",
    )?;
    source_session(
        &fixture,
        73_006,
        "/tmp/cass/truncated",
        &[
            "Observation zeta.".to_owned(),
            format!("{anchor_text}\n[TRUNCATED]"),
        ],
        "",
    )?;
    source_session(
        &fixture,
        73_007,
        "/tmp/cass/refuted-unicode",
        &[
            "Observation eta.".to_owned(),
            anchor_text,
            "This advice didn’t fix it.".to_owned(),
        ],
        "",
    )?;
    let observed = propose(&fixture, &anchor, 0.0, true)?;
    let other = propose(&fixture, &changed, 0.0, false)?;
    assert_eq!(
        observed.candidates[0].proposed_content, other.candidates[0].proposed_content,
        "the same short display cannot authenticate different operational tails"
    );
    assert_eq!(observed.candidates[0].confidence, 0.6);
    assert_eq!(propose(&fixture, &anchor, 0.7, false)?.candidate_count, 0);
    assert!(
        propose(&fixture, &rejected, 0.0, false)?
            .candidates
            .iter()
            .all(|candidate| candidate.confidence <= 0.6)
    );
    Ok(())
}

#[test]
fn recorded_proof_binds_content_score_package_and_captured_source_history() -> TestResult {
    let fixture = review_session_fixture()?;
    let anchor = independent_session(&fixture, 74_001)?;
    let supporter = independent_session(&fixture, 74_002)?;
    independent_session(&fixture, 74_003)?;
    let candidate = propose(&fixture, &anchor, 0.8, true)?.candidates.remove(0);
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let stored = stored_candidate(&connection, &fixture, &candidate.candidate_id)?;
    for modification in [
        "content",
        "score",
        "memory_spec",
        "source_package",
        "missing_proof",
        "missing_proof_stored",
        "missing_proof_proposed",
        "missing_proof_memory_spec",
    ] {
        let mut forged = stored.clone();
        match modification {
            "content" => {
                forged.proposed_content = Some(
                    "Always skip cargo test --locked after changing src/support.rs.".to_owned(),
                )
            }
            "score" => {
                forged.confidence = 0.99;
                forged.proposed_confidence = Some(0.99);
            }
            "memory_spec" => {
                let mut metadata: serde_json::Value = serde_json::from_str(
                    forged
                        .derivation_metadata_json
                        .as_deref()
                        .expect("metadata"),
                )
                .map_err(|error| error.to_string())?;
                metadata["memorySpec"]["confidence"] = serde_json::json!(0.99);
                forged.derivation_metadata_json = Some(metadata.to_string());
            }
            "missing_proof"
            | "missing_proof_stored"
            | "missing_proof_proposed"
            | "missing_proof_memory_spec" => {
                let mut metadata: serde_json::Value = serde_json::from_str(
                    forged
                        .derivation_metadata_json
                        .as_deref()
                        .expect("metadata"),
                )
                .map_err(|error| error.to_string())?;
                metadata["producer"]["producerPayload"]
                    .as_object_mut()
                    .expect("payload")
                    .remove("corroboration");
                if modification != "missing_proof" {
                    forged.confidence = 0.6;
                    forged.proposed_confidence = Some(0.6);
                    metadata["memorySpec"]["confidence"] = serde_json::json!(0.6);
                    match modification {
                        "missing_proof_stored" => forged.confidence = 0.8,
                        "missing_proof_proposed" => forged.proposed_confidence = Some(0.8),
                        _ => metadata["memorySpec"]["confidence"] = serde_json::json!(0.8),
                    }
                }
                forged.derivation_metadata_json = Some(metadata.to_string());
            }
            _ => forged.source_id = Some(evidence_id(74_999)),
        }
        let issue = corroboration::validate_recorded(&connection, &forged).expect_err(modification);
        assert_eq!(issue.code, "review_corroboration_changed");
    }
    let mut unproved_draft = candidate.clone();
    unproved_draft.corroboration = None;
    assert!(
        corroboration::validate_draft(&connection, &fixture.workspace_id, &unproved_draft).is_err()
    );
    let mut unproved_metadata: serde_json::Value = serde_json::from_str(
        stored
            .derivation_metadata_json
            .as_deref()
            .expect("metadata"),
    )
    .map_err(|error| error.to_string())?;
    unproved_metadata["producer"]["producerPayload"]
        .as_object_mut()
        .expect("payload")
        .remove("corroboration");
    let mut historical = stored.clone();
    historical.confidence = 0.55;
    historical.proposed_confidence = Some(0.55);
    let mut old_metadata = unproved_metadata.clone();
    old_metadata["memorySpec"]["confidence"] = serde_json::json!(0.55);
    historical.derivation_metadata_json = Some(old_metadata.to_string());
    corroboration::validate_recorded(&connection, &historical).map_err(|error| error.message)?;
    let mut fake_legacy_arc = stored.clone();
    fake_legacy_arc.confidence = 0.82;
    fake_legacy_arc.proposed_confidence = Some(0.82);
    let mut fake_arc_metadata = unproved_metadata.clone();
    fake_arc_metadata["memorySpec"]["confidence"] = serde_json::json!(0.82);
    fake_arc_metadata["producer"]["producerPayload"]["sessionArc"] = serde_json::json!({});
    fake_legacy_arc.derivation_metadata_json = Some(fake_arc_metadata.to_string());
    assert!(
        corroboration::validate_recorded(&connection, &fake_legacy_arc).is_err(),
        "a marker cannot turn a bootstrap into a historical arc package"
    );
    let mut other_producer = stored.clone();
    let mut other_metadata = unproved_metadata.clone();
    other_metadata["producer"]["producer"] = serde_json::json!("test-reflector");
    other_producer.derivation_metadata_json = Some(other_metadata.to_string());
    corroboration::validate_recorded(&connection, &other_producer)
        .map_err(|error| error.message)?;
    // A live proof-stripping mutation must fail public validation even though
    // the lesson, source references and all original source rows are unchanged.
    connection
        .execute_raw(&format!(
            "UPDATE curation_candidates SET derivation_metadata_json = '{}' WHERE id = '{}'",
            unproved_metadata.to_string().replace('\'', "''"),
            candidate.candidate_id,
        ))
        .map_err(|error| error.to_string())?;
    let refused = validate_curation_candidate(&crate::core::curate::CurateValidateOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        candidate_id: &candidate.candidate_id,
        actor: Some("CorroborationTest"),
        dry_run: true,
    })
    .map_err(|error| error.message())?;
    assert!(
        refused
            .validation
            .errors
            .iter()
            .any(|issue| issue.code == "review_corroboration_changed")
    );
    assert!(!refused.durable_mutation);
    // A separate, unchanged proposal witnesses the same captured source. A
    // later edit to an observation within that prefix must block its apply.
    let other = propose(&fixture, &supporter, 0.8, true)?
        .candidates
        .remove(0);
    validate_arc(&fixture, &other.candidate_id)?;
    let count = connection
        .list_memories(&fixture.workspace_id, None, true)
        .map_err(|error| error.to_string())?
        .len();
    let audits = connection
        .list_audit_entries(Some(&fixture.workspace_id), None)
        .map_err(|error| error.to_string())?
        .len();
    connection
        .execute_raw(&format!(
            "UPDATE evidence_spans SET cass_span_id = 'changed-original-source' WHERE id = '{}'",
            evidence_id(74_001 * 100),
        ))
        .map_err(|error| error.to_string())?;
    let refused = apply_arc(&fixture, &other.candidate_id, false)?;
    assert_eq!(refused.application.status, "blocked");
    assert!(
        refused
            .application
            .errors
            .iter()
            .any(|issue| issue.code == "review_corroboration_changed")
    );
    assert!(!refused.durable_mutation);
    assert_eq!(
        connection
            .list_memories(&fixture.workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .len(),
        count
    );
    assert_eq!(
        connection
            .list_audit_entries(Some(&fixture.workspace_id), None)
            .map_err(|error| error.to_string())?
            .len(),
        audits
    );
    assert_eq!(
        stored_candidate(&connection, &fixture, &other.candidate_id)?.status,
        "approved"
    );
    Ok(())
}

#[test]
fn rejected_support_is_not_replaced_or_counted_as_a_new_observation() -> TestResult {
    let fixture = review_session_fixture()?;
    let anchor = independent_session(&fixture, 75_001)?;
    let supporter = independent_session(&fixture, 75_002)?;
    let remaining = independent_session(&fixture, 75_003)?;
    let first = propose(&fixture, &anchor, 0.8, true)?.candidates.remove(0);
    let rejected = propose(&fixture, &supporter, 0.8, true)?
        .candidates
        .remove(0);
    review_curation_candidate(&CurateReviewOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        candidate_id: &rejected.candidate_id,
        action: CurateReviewAction::Reject,
        actor: Some("CorroborationTest"),
        dry_run: false,
        snoozed_until: None,
        reason: Some("This observation does not support the proposed lesson."),
        merge_into_candidate_id: None,
    })
    .map_err(|error| error.message())?;
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let stored = stored_candidate(&connection, &fixture, &first.candidate_id)?;
    assert!(corroboration::validate_recorded(&connection, &stored).is_err());
    let fresh = propose(&fixture, &remaining, 0.0, true)?;
    assert_eq!(fresh.candidates[0].confidence, 0.7);
    assert_eq!(
        proof(&stored_candidate(
            &connection,
            &fixture,
            &fresh.candidates[0].candidate_id
        )?)["sessions"]
            .as_array()
            .expect("sessions")
            .len(),
        2
    );
    assert_eq!(
        stored_candidate(&connection, &fixture, &first.candidate_id)?.derivation_metadata_json,
        stored.derivation_metadata_json,
        "a disappeared supporter is never silently replaced"
    );
    Ok(())
}

#[test]
fn session_read_budget_counts_utf8_metadata_and_generated_proof_fits_revalidation() -> TestResult {
    let fixture = review_session_fixture()?;
    let padding = "λ".repeat(900_000);
    let mut sessions = Vec::new();
    for seed in 76_001..=76_003 {
        sessions.push(source_session(
            &fixture,
            seed,
            &format!("/tmp/cass/large-{seed}"),
            &[
                format!("Observation from session {seed}."),
                LESSON.to_owned(),
            ],
            &padding,
        )?);
    }
    let report = propose(&fixture, &sessions[0], 0.7, true)?;
    assert_eq!(
        report.candidate_count, 1,
        "a bounded proof must be persistable in its own transaction"
    );
    assert_eq!(
        report.candidates[0].confidence, 0.7,
        "one shared 8 MiB budget cannot hold three large sessions"
    );
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let stored = stored_candidate(&connection, &fixture, &report.candidates[0].candidate_id)?;
    corroboration::validate_recorded(&connection, &stored).map_err(|error| error.message)?;
    connection
        .begin_read_snapshot()
        .map_err(|error| error.to_string())?;
    let limits = ReviewCorroborationLimits {
        sessions: 1,
        spans_per_session: 8,
        total_bytes: 8 * 1024 * 1024,
    };
    let loaded = connection
        .review_corroboration_sessions(&fixture.workspace_id, &sessions[0], &[&sessions[0]], limits)
        .map_err(|error| error.to_string())?;
    let (_, rows, accounted) = loaded.first().expect("one complete session");
    assert_eq!(rows.len(), 2);
    assert!(
        *accounted >= padding.len() as u64 * 2,
        "byte accounting must include UTF-8 metadata, not its character count"
    );
    let too_small = connection
        .review_corroboration_sessions(
            &fixture.workspace_id,
            &sessions[0],
            &[&sessions[0]],
            ReviewCorroborationLimits {
                total_bytes: accounted - 1,
                ..limits
            },
        )
        .map_err(|error| error.to_string())?;
    assert!(too_small.is_empty());
    let partial = connection
        .review_corroboration_sessions(
            &fixture.workspace_id,
            &sessions[0],
            &[&sessions[0]],
            ReviewCorroborationLimits {
                spans_per_session: 1,
                ..limits
            },
        )
        .map_err(|error| error.to_string())?;
    assert!(
        partial.is_empty(),
        "a partial transcript must never masquerade as complete support"
    );
    let foreign = connection
        .review_corroboration_sessions(
            "wsp_other00000000000000000000",
            &sessions[0],
            &[&sessions[0]],
            limits,
        )
        .map_err(|error| error.to_string())?;
    assert!(foreign.is_empty());
    connection
        .commit_read_snapshot()
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn historical_arc_082_package_remains_exact_without_accepting_other_unproved_scores() -> TestResult
{
    let (fixture, candidates) = arc_fixture(false)?;
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    for candidate in &candidates {
        let mut historical = candidate.clone();
        historical.confidence = 0.82;
        historical.proposed_confidence = 0.82;
        historical.corroboration = None;
        let (_, metadata) = crate::core::curate::review_bootstrap_derivation_package(
            &connection,
            &fixture.workspace_id,
            &historical,
            None,
        )
        .map_err(|error| error.message())?;
        connection.execute_raw(&format!(
            "UPDATE curation_candidates SET confidence = 0.82, proposed_confidence = 0.82, derivation_metadata_json = '{}' WHERE id = '{}'",
            metadata.replace('\'', "''"), candidate.candidate_id,
        )).map_err(|error| error.to_string())?;
    }
    let before = stored_candidate(&connection, &fixture, &candidates[0].candidate_id)?;
    assert!(
        !before
            .derivation_metadata_json
            .as_deref()
            .expect("metadata")
            .contains("corroboration")
    );
    corroboration::validate_recorded(&connection, &before).map_err(|issue| issue.message)?;
    session_arc::applied_peer(&connection, &before).map_err(|issue| issue.message)?;
    let mut forged = before.clone();
    let mut metadata: serde_json::Value = serde_json::from_str(
        forged
            .derivation_metadata_json
            .as_deref()
            .expect("metadata"),
    )
    .map_err(|error| error.to_string())?;
    forged.confidence = 0.91;
    forged.proposed_confidence = Some(0.91);
    metadata["memorySpec"]["confidence"] = serde_json::json!(0.91);
    forged.derivation_metadata_json = Some(metadata.to_string());
    assert!(
        session_arc::applied_peer(&connection, &forged).is_err(),
        "arbitrary proofless confidence cannot pass as historical producer output"
    );
    validate_arc(&fixture, &candidates[0].candidate_id)?;
    let first = apply_arc(&fixture, &candidates[0].candidate_id, false)?;
    assert_eq!(
        first.application.status, "applied",
        "{:?}",
        first.application.errors
    );
    validate_arc(&fixture, &candidates[1].candidate_id)?;
    let second = apply_arc(&fixture, &candidates[1].candidate_id, false)?;
    assert_eq!(
        second.application.status, "applied",
        "{:?}",
        second.application.errors
    );
    let after = stored_candidate(&connection, &fixture, &candidates[0].candidate_id)?;
    assert_eq!(after.confidence, 0.82);
    assert_eq!(
        after.derivation_metadata_json,
        before.derivation_metadata_json
    );
    assert_eq!(
        after.derivation_source_refs_json,
        before.derivation_source_refs_json
    );
    assert_eq!(
        apply_arc(&fixture, &candidates[0].candidate_id, false)?
            .application
            .status,
        "already_applied"
    );
    Ok(())
}
