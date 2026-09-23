use super::*;
use crate::core::curate::session_arc;

const INLINE_ARC: &str = "Failure arc: storing silently would violate the no-loop-takeover policy.\nFix: require accept/reject commands and audit every accepted capture.";

#[test]
fn multi_episode_acceptance_keeps_first_owner_and_links_only_the_current_pair() -> TestResult {
    let fixture = review_session_fixture()?;
    let session = SessionId::from_uuid(uuid::Uuid::from_u128(9820)).to_string();
    let source_id = evidence_id(9821);
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    connection
        .insert_session(
            &session,
            &session_input(&fixture.workspace_id, "multi-episode-acceptance"),
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_evidence_span(
            &source_id,
            &evidence_span_input(
                &fixture.workspace_id,
                &session,
                None,
                "multi-episode-acceptance-span",
                60,
                &format!("{INLINE_ARC}\n{LATER_FAILURE}\n{LATER_REPAIR}"),
            ),
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;

    let report = review_session_proposals(&ReviewSessionOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        session_id: Some(&session),
        propose: true,
        dry_run: false,
        min_confidence: 0.8,
        limit: 4,
    })
    .map_err(|error| error.message())?;
    assert_eq!(report.candidate_count, 4, "{:?}", report.candidates);
    assert!(
        report
            .candidates
            .iter()
            .all(|candidate| candidate.persisted)
    );

    let mut first_pair = Vec::new();
    let mut later_pair = Vec::new();
    for candidate in &report.candidates {
        let arc = candidate.session_arc.as_ref().expect("session arc");
        if arc
            .failure_span
            .excerpt
            .contains("rewriting evidence ownership")
        {
            later_pair.push(candidate);
        } else {
            first_pair.push(candidate);
        }
    }
    assert_eq!(first_pair.len(), 2);
    assert_eq!(later_pair.len(), 2);

    validate_arc(&fixture, &first_pair[0].candidate_id)?;
    let owner = apply_arc(&fixture, &first_pair[0].candidate_id, false)?
        .application
        .created_memory_id
        .expect("first accepted memory");

    validate_arc(&fixture, &later_pair[0].candidate_id)?;
    let later_first = apply_arc(&fixture, &later_pair[0].candidate_id, false)?
        .application
        .created_memory_id
        .expect("first later memory");

    validate_arc(&fixture, &later_pair[1].candidate_id)?;
    let shown = super::super::show_curation_candidate(&super::super::CurateShowOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        candidate_id: &later_pair[1].candidate_id,
    })
    .map_err(|error| error.message())?;
    let plan = shown.planned_application.expect("application preview");
    assert_eq!(plan.status, "ready", "{:?}", plan.errors);
    assert!(plan.planned_evidence_attachments.is_empty());
    assert_eq!(plan.shared_evidence_spans.len(), 1);
    assert_eq!(plan.shared_evidence_spans[0].evidence_span_id, source_id);
    assert_eq!(plan.shared_evidence_spans[0].owner_memory_id, owner);
    let planned_link = plan
        .planned_session_arc_link
        .expect("only the current reciprocal pair should link");
    assert!(planned_link.src_memory_id == later_first || planned_link.dst_memory_id == later_first);
    assert_ne!(planned_link.src_memory_id, owner);
    assert_ne!(planned_link.dst_memory_id, owner);

    let later_second = apply_arc(&fixture, &later_pair[1].candidate_id, false)?
        .application
        .created_memory_id
        .expect("second later memory");

    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    assert_eq!(
        connection
            .get_evidence_span(&source_id)
            .map_err(|error| error.to_string())?
            .expect("shared source")
            .memory_id
            .as_deref(),
        Some(owner.as_str()),
        "later learning must never rewrite the first accepted evidence owner"
    );
    assert_eq!(
        connection
            .get_curation_candidate(&fixture.workspace_id, &first_pair[1].candidate_id)
            .map_err(|error| error.to_string())?
            .expect("unapplied reciprocal candidate")
            .status,
        "pending",
        "accepting a different episode must not implicitly accept this one"
    );
    assert!(
        connection
            .list_memory_links_for_memory(&owner, None)
            .map_err(|error| error.to_string())?
            .is_empty(),
        "shared provenance must not manufacture a cross-episode memory link"
    );
    let links = connection
        .list_memory_links_for_memory(&later_first, Some(MemoryLinkRelation::Related))
        .map_err(|error| error.to_string())?;
    assert_eq!(links.len(), 1);
    let link = &links[0];
    assert!(
        (link.src_memory_id == later_first && link.dst_memory_id == later_second)
            || (link.src_memory_id == later_second && link.dst_memory_id == later_first)
    );
    assert_eq!(
        connection
            .list_audit_by_target("memory_link", &link.id, None)
            .map_err(|error| error.to_string())?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn session_arc_within_one_window_retains_exact_source_identity() {
    let session = synthetic_stored_session();
    let span = synthetic_span("ev_arc_window", None, INLINE_ARC);
    let candidates = super::super::build_session_arc_candidates(
        &session.workspace_id,
        &session,
        std::slice::from_ref(&span),
        0.0,
    );
    assert_eq!(candidates.len(), 2);
    for candidate in &candidates {
        assert_eq!(candidate.source_ids, vec![span.id.clone()]);
        let arc = candidate.session_arc.as_ref().unwrap();
        assert_eq!(arc.failure_span.evidence_span_id, span.id);
        assert_eq!(arc.resolution_span.evidence_span_id, span.id);
        assert_eq!(arc.failure_span.content_hash, span.content_hash);
        assert_eq!(arc.resolution_span.content_hash, span.content_hash);
        assert_eq!(arc.failure_span.start_line, span.start_line);
        assert_eq!(arc.resolution_span.end_line, span.end_line);
        assert_eq!(
            arc.failure_span.provenance_uri,
            span.canonical_provenance_uri()
        );
        assert_eq!(
            arc.resolution_span.provenance_uri,
            span.canonical_provenance_uri()
        );
        assert_ne!(arc.failure_span.excerpt, arc.resolution_span.excerpt);
    }
    assert_eq!(
        super::super::capture_suggestion_memory_kind(&candidates[0]),
        "anti-pattern"
    );
    assert_eq!(
        super::super::capture_suggestion_memory_kind(&candidates[1]),
        "rule"
    );
}

#[test]
fn session_arc_limit_never_splits_a_pair() {
    let session = synthetic_stored_session();
    let span = synthetic_span("ev_arc_window", None, INLINE_ARC);
    for limit in 0..5 {
        let mut candidates = super::super::build_session_arc_candidates(
            &session.workspace_id,
            &session,
            std::slice::from_ref(&span),
            0.0,
        );
        session_arc::limit_complete_pairs(&mut candidates, limit);
        assert_eq!(candidates.len(), if limit < 2 { 0 } else { 2 });
    }
}

#[test]
fn session_arc_rejects_negative_repairs_and_reversed_order() {
    let session = synthetic_stored_session();
    for text in [
        "cargo test failed. cargo test is not fixed.",
        "cargo test failed. cargo test never passed.",
        "cargo test failed. cargo test fix failed.",
        "cargo test passed. cargo test failed.",
        "cargo test failed but the word fixed appears in the same clause",
    ] {
        let span = synthetic_span("ev_negative", None, text);
        assert!(
            super::super::build_session_arc_candidates(
                &session.workspace_id,
                &session,
                &[span],
                0.0
            )
            .is_empty(),
            "{text}"
        );
    }
    assert!(session_arc::resolution_signal(
        "cargo test passed with no errors."
    ));
}

#[test]
fn session_arc_does_not_join_unrelated_topics_or_foreign_windows() {
    let session = synthetic_stored_session();
    let span = synthetic_span(
        "ev_unrelated",
        None,
        "Database migration failed. cargo fmt fixed the formatting.",
    );
    assert!(session_arc::inline_candidates(&session.workspace_id, &session, &[span]).is_empty());
    let mut foreign = synthetic_span("ev_foreign", None, INLINE_ARC);
    foreign.session_id = "ses_other".to_owned();
    assert!(session_arc::inline_candidates(&session.workspace_id, &session, &[foreign]).is_empty());
}

// bd-6br0o: the capture fixture's two halves arrive as SEPARATE transcript
// lines, so inline_pair never sees them together. They key to different topics
// ("failure" vs "accept"), so the grouped pass never compares them either, and
// the capture e2e was red as a result. The explicit marker pair is the
// product's own declaration that these two excerpts form an arc.
#[test]
fn session_arc_explicit_marker_pairs_across_two_spans() {
    let session = synthetic_stored_session();
    let mut failure = super::super::tests::synthetic_span(
        "ev_marked_failure",
        None,
        "Failure arc: storing silently would violate the no-loop-takeover policy.",
    );
    let mut repair = super::super::tests::synthetic_span(
        "ev_marked_fix",
        None,
        "Fix: require accept/reject commands and audit every accepted capture.",
    );
    failure.start_line = 3;
    failure.end_line = 3;
    repair.start_line = 4;
    repair.end_line = 4;
    let candidates = super::super::build_session_arc_candidates(
        &session.workspace_id,
        &session,
        &[failure, repair],
        0.0,
    );
    assert_eq!(
        candidates.len(),
        2,
        "an explicit `Failure arc:`/`Fix:` pair split across two spans must still \
         produce a reciprocal arc; got {} candidate(s)",
        candidates.len()
    );
}

// A repair that pairs everything is worse than one that pairs nothing. Same two
// spans, same line ordering, same unrelated topics -- with the explicit markers
// REMOVED. This must still refuse to pair. Green both before and after the
// bd-6br0o repair, by design: it guards against over-pairing rather than
// discriminating the fix.
#[test]
fn session_arc_without_markers_still_refuses_unrelated_topics() {
    let session = synthetic_stored_session();
    let mut failure = super::super::tests::synthetic_span(
        "ev_plain_failure",
        None,
        "Storing silently would violate the no-loop-takeover policy and failed.",
    );
    let mut repair = super::super::tests::synthetic_span(
        "ev_plain_fix",
        None,
        "Fixed by requiring accept/reject commands and auditing every capture.",
    );
    failure.start_line = 3;
    failure.end_line = 3;
    repair.start_line = 4;
    repair.end_line = 4;
    let candidates = super::super::build_session_arc_candidates(
        &session.workspace_id,
        &session,
        &[failure, repair],
        0.0,
    );
    assert!(
        candidates.is_empty(),
        "unmarked spans on different topics must NOT pair; got {} candidate(s)",
        candidates.len()
    );
}

#[test]
fn session_arc_distinct_overlapping_windows_do_not_invent_temporal_order() {
    let session = synthetic_stored_session();
    let mut failure = synthetic_span("ev_failure", None, "cargo test failed.");
    let mut repair = synthetic_span("ev_repair", None, "cargo test passed.");
    failure.start_line = 10;
    failure.end_line = 20;
    repair.start_line = 15;
    repair.end_line = 25;
    assert!(
        super::super::build_session_arc_candidates(
            &session.workspace_id,
            &session,
            &[failure, repair],
            0.0
        )
        .is_empty()
    );
}

fn arc_fixture(
    single_window: bool,
) -> Result<(ReviewFixture, Vec<super::super::ReviewSessionCandidate>), String> {
    let fixture = review_session_fixture()?;
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let session = SessionId::from_uuid(uuid::Uuid::from_u128(9801)).to_string();
    connection
        .insert_session(
            &session,
            &session_input(&fixture.workspace_id, "session-arc-learning"),
        )
        .map_err(|error| error.to_string())?;
    let excerpts: Vec<_> = if single_window {
        vec![INLINE_ARC]
    } else {
        vec![
            "The capture hook cargo test failed with red error output.",
            "Fixed the capture hook and cargo test passed green.",
        ]
    };
    for (index, excerpt) in excerpts.into_iter().enumerate() {
        connection
            .insert_evidence_span(
                &evidence_id(9802 + index as u128),
                &evidence_span_input(
                    &fixture.workspace_id,
                    &session,
                    None,
                    &format!("arc-span-{index}"),
                    60 + u32::try_from(index).unwrap() * 10,
                    excerpt,
                ),
            )
            .map_err(|error| error.to_string())?;
    }
    connection.close().map_err(|error| error.to_string())?;
    let report = review_session_proposals(&ReviewSessionOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        session_id: Some(&session),
        propose: true,
        dry_run: false,
        min_confidence: 0.8,
        limit: 2,
    })
    .map_err(|error| error.message())?;
    assert_eq!(
        report.candidate_count, 2,
        "an entire arc pair must be persisted: {:?}",
        report.candidates
    );
    assert!(
        report
            .candidates
            .iter()
            .all(|candidate| candidate.persisted && candidate.session_arc.is_some())
    );
    Ok((fixture, report.candidates))
}

fn validate_arc(fixture: &ReviewFixture, id: &str) -> TestResult {
    let report = validate_curation_candidate(&super::super::CurateValidateOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        candidate_id: id,
        actor: Some("ArcLearner"),
        dry_run: false,
    })
    .map_err(|error| error.message())?;
    assert!(
        report.validation.errors.is_empty(),
        "{:?}",
        report.validation.errors
    );
    Ok(())
}

fn apply_arc(
    fixture: &ReviewFixture,
    id: &str,
    dry_run: bool,
) -> Result<super::super::CurateApplyReport, String> {
    apply_curation_candidate(&super::super::CurateApplyOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        candidate_id: id,
        actor: Some("ArcLearner"),
        dry_run,
        allow_tombstone_load_bearing: false,
    })
    .map_err(|error| error.message())
}

fn exercise_pair(single_window: bool, rule_first: bool) -> TestResult {
    let (fixture, mut candidates) = arc_fixture(single_window)?;
    candidates.sort_by_key(|candidate| {
        candidate.candidate_kind == super::super::REVIEW_CANDIDATE_KIND_SESSION_ARC_RULE
    });
    if rule_first {
        candidates.reverse();
    }
    let first = &candidates[0];
    let second = &candidates[1];
    validate_arc(&fixture, &first.candidate_id)?;
    let first_apply = apply_arc(&fixture, &first.candidate_id, false)?;
    assert_eq!(
        first_apply.application.status, "applied",
        "{:?}",
        first_apply.application.errors
    );
    let first_memory_id = first_apply.application.created_memory_id.unwrap();
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    assert_eq!(
        connection
            .get_curation_candidate(&fixture.workspace_id, &second.candidate_id)
            .map_err(|error| error.to_string())?
            .unwrap()
            .status,
        "pending",
        "learning one side must not accept the other"
    );
    assert!(
        connection
            .list_memory_links_for_memory(&first_memory_id, None)
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    let before = connection
        .list_memories(&fixture.workspace_id, None, false)
        .map_err(|error| error.to_string())?
        .len();
    validate_arc(&fixture, &second.candidate_id)?;
    let preview = apply_arc(&fixture, &second.candidate_id, true)?;
    assert!(!preview.mutation.persisted);
    assert!(
        preview.application.errors.is_empty(),
        "{:?}",
        preview.application.errors
    );
    assert_eq!(
        connection
            .list_memories(&fixture.workspace_id, None, false)
            .map_err(|error| error.to_string())?
            .len(),
        before
    );
    let applied = apply_arc(&fixture, &second.candidate_id, false)?;
    assert_eq!(
        applied.application.status, "applied",
        "{:?}",
        applied.application.errors
    );
    let second_memory_id = applied.application.created_memory_id.unwrap();
    let first_memory = connection
        .get_memory(&first_memory_id)
        .map_err(|error| error.to_string())?
        .unwrap();
    let second_memory = connection
        .get_memory(&second_memory_id)
        .map_err(|error| error.to_string())?
        .unwrap();
    assert_eq!(
        first_memory.kind,
        super::super::review_candidate_derived_memory_kind(first)
    );
    assert_eq!(
        second_memory.kind,
        super::super::review_candidate_derived_memory_kind(second)
    );
    assert_eq!(first_memory.level, "procedural");
    assert_eq!(second_memory.level, "procedural");
    for id in &first.source_ids {
        let span = connection
            .get_evidence_span(id)
            .map_err(|error| error.to_string())?
            .unwrap();
        assert_eq!(
            span.memory_id.as_deref(),
            Some(first_memory_id.as_str()),
            "the first source owner must not be overwritten"
        );
    }
    let links = connection
        .list_memory_links_for_memory(&first_memory_id, Some(MemoryLinkRelation::Related))
        .map_err(|error| error.to_string())?;
    assert_eq!(links.len(), 1);
    let link = &links[0];
    assert!(!link.directed);
    assert_eq!(
        link.src_memory_id,
        if rule_first {
            first_memory_id.clone()
        } else {
            second_memory_id.clone()
        }
    );
    let metadata: serde_json::Value = serde_json::from_str(link.metadata_json.as_deref().unwrap())
        .map_err(|error| error.to_string())?;
    assert_eq!(metadata["linkage"], "failed_to_fixed");
    assert_eq!(
        metadata["arcId"],
        first.session_arc.as_ref().unwrap().arc_id
    );
    let link_audits = connection
        .list_audit_by_target("memory_link", &link.id, None)
        .map_err(|error| error.to_string())?;
    assert_eq!(link_audits.len(), 1);
    assert_eq!(link_audits[0].action, audit_actions::MEMORY_LINK_CREATE);
    assert_eq!(link_audits[0].actor.as_deref(), Some("ArcLearner"));
    let memory_audits = connection
        .list_audit_by_action(audit_actions::MEMORY_CREATE, None)
        .map_err(|error| error.to_string())?;
    for memory_id in [&first_memory_id, &second_memory_id] {
        let audit = memory_audits
            .iter()
            .find(|entry| entry.target_id.as_ref() == Some(memory_id))
            .unwrap();
        let details: serde_json::Value = serde_json::from_str(audit.details.as_deref().unwrap())
            .map_err(|error| error.to_string())?;
        assert_eq!(
            details["sourceRefs"].as_array().unwrap().len(),
            first.source_ids.len()
        );
        assert_eq!(
            details["producerPayload"]["sessionArc"]["arcId"],
            first.session_arc.as_ref().unwrap().arc_id
        );
    }
    let audit_count = connection
        .list_audit_entries(Some(&fixture.workspace_id), None)
        .map_err(|error| error.to_string())?
        .len();
    for candidate in &candidates {
        assert_eq!(
            apply_arc(&fixture, &candidate.candidate_id, false)?
                .application
                .status,
            "already_applied"
        );
    }
    assert_eq!(
        connection
            .list_audit_entries(Some(&fixture.workspace_id), None)
            .map_err(|error| error.to_string())?
            .len(),
        audit_count
    );
    assert_eq!(
        connection
            .list_memories(&fixture.workspace_id, None, false)
            .map_err(|error| error.to_string())?
            .len(),
        before + 1
    );
    Ok(())
}

#[test]
fn session_arc_accepts_rule_then_anti_pattern_with_audited_link() -> TestResult {
    exercise_pair(false, true)
}

#[test]
fn session_arc_accepts_anti_pattern_then_rule_with_audited_link() -> TestResult {
    exercise_pair(false, false)
}

#[test]
fn session_arc_single_window_pair_round_trips_through_real_curation() -> TestResult {
    exercise_pair(true, false)
}

#[test]
fn session_arc_rejecting_peer_never_creates_or_accepts_it() -> TestResult {
    let (fixture, candidates) = arc_fixture(true)?;
    let report = review_curation_candidate(&super::super::CurateReviewOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        candidate_id: &candidates[1].candidate_id,
        action: CurateReviewAction::Reject,
        actor: Some("ArcLearner"),
        dry_run: false,
        snoozed_until: None,
        reason: Some("Keep only the failure lesson."),
        merge_into_candidate_id: None,
    })
    .map_err(|error| error.message())?;
    assert!(report.mutation.persisted);
    validate_arc(&fixture, &candidates[0].candidate_id)?;
    let applied = apply_arc(&fixture, &candidates[0].candidate_id, false)?;
    assert_eq!(applied.application.status, "applied");
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    assert_eq!(
        connection
            .get_curation_candidate(&fixture.workspace_id, &candidates[1].candidate_id)
            .map_err(|error| error.to_string())?
            .unwrap()
            .status,
        "rejected"
    );
    assert!(
        connection
            .list_memory_links_for_memory(
                applied.application.created_memory_id.as_deref().unwrap(),
                None
            )
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    Ok(())
}

#[test]
fn session_arc_retired_peer_fails_closed_without_partial_learning() -> TestResult {
    let (fixture, candidates) = arc_fixture(false)?;
    validate_arc(&fixture, &candidates[0].candidate_id)?;
    let first = apply_arc(&fixture, &candidates[0].candidate_id, false)?
        .application
        .created_memory_id
        .unwrap();
    validate_arc(&fixture, &candidates[1].candidate_id)?;
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    connection
        .tombstone_memory(&first)
        .map_err(|error| error.to_string())?;
    let count = connection
        .list_memories(&fixture.workspace_id, None, true)
        .map_err(|error| error.to_string())?
        .len();
    let report = apply_arc(&fixture, &candidates[1].candidate_id, false)?;
    assert_eq!(report.application.status, "blocked");
    assert!(!report.mutation.persisted);
    assert!(
        report
            .application
            .errors
            .iter()
            .any(|issue| issue.code == "session_arc_pair_invalid")
    );
    assert_eq!(
        connection
            .list_memories(&fixture.workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .len(),
        count
    );
    assert!(
        connection
            .list_memory_links_for_memory(&first, None)
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    Ok(())
}

#[test]
fn session_arc_forged_identity_or_role_cannot_reuse_owned_evidence() -> TestResult {
    let (fixture, candidates) = arc_fixture(false)?;
    validate_arc(&fixture, &candidates[0].candidate_id)?;
    apply_arc(&fixture, &candidates[0].candidate_id, false)?;
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let stored = connection
        .get_curation_candidate(&fixture.workspace_id, &candidates[1].candidate_id)
        .map_err(|error| error.to_string())?
        .unwrap();
    for field in ["linkedCandidateId", "role", "arcId"] {
        let mut forged = stored.clone();
        let mut metadata: serde_json::Value =
            serde_json::from_str(forged.derivation_metadata_json.as_deref().unwrap())
                .map_err(|error| error.to_string())?;
        metadata["producer"]["producerPayload"]["sessionArc"][field] = serde_json::json!("forged");
        forged.derivation_metadata_json = Some(metadata.to_string());
        assert!(
            session_arc::applied_peer(&connection, &forged).is_err(),
            "{field}"
        );
    }
    let mut unrelated = stored.clone();
    unrelated.id = curate_id(9900);
    assert!(session_arc::applied_peer(&connection, &unrelated).is_err());
    Ok(())
}

#[test]
fn session_arc_link_and_audit_roll_back_when_second_apply_fails() -> TestResult {
    let (fixture, candidates) = arc_fixture(false)?;
    validate_arc(&fixture, &candidates[0].candidate_id)?;
    let first = apply_arc(&fixture, &candidates[0].candidate_id, false)?
        .application
        .created_memory_id
        .unwrap();
    validate_arc(&fixture, &candidates[1].candidate_id)?;
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let count = connection
        .list_memories(&fixture.workspace_id, None, true)
        .map_err(|error| error.to_string())?
        .len();
    let audits = connection
        .list_audit_entries(Some(&fixture.workspace_id), None)
        .map_err(|error| error.to_string())?
        .len();
    super::super::set_create_derived_apply_fail_phase(Some("before_insert_search_index_job"));
    let result = apply_arc(&fixture, &candidates[1].candidate_id, false);
    super::super::set_create_derived_apply_fail_phase(None);
    assert!(
        result.is_err(),
        "the injected failure must execute after link and link audit creation"
    );
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
    assert!(
        connection
            .list_memory_links_for_memory(&first, None)
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    assert_eq!(
        connection
            .get_curation_candidate(&fixture.workspace_id, &candidates[1].candidate_id)
            .map_err(|error| error.to_string())?
            .unwrap()
            .status,
        "approved"
    );
    let retry = apply_arc(&fixture, &candidates[1].candidate_id, false)?;
    assert_eq!(
        retry.application.status, "applied",
        "{:?}",
        retry.application.errors
    );
    assert_eq!(
        connection
            .list_memory_links_for_memory(&first, None)
            .map_err(|error| error.to_string())?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn session_arc_preview_discloses_link_and_does_not_promise_source_reassignment() -> TestResult {
    let (fixture, candidates) = arc_fixture(false)?;
    validate_arc(&fixture, &candidates[0].candidate_id)?;
    let first = apply_arc(&fixture, &candidates[0].candidate_id, false)?
        .application
        .created_memory_id
        .unwrap();
    validate_arc(&fixture, &candidates[1].candidate_id)?;
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    let audits = connection
        .list_audit_entries(Some(&fixture.workspace_id), None)
        .map_err(|error| error.to_string())?
        .len();
    let shown = super::super::show_curation_candidate(&super::super::CurateShowOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        candidate_id: &candidates[1].candidate_id,
    })
    .map_err(|error| error.message())?;
    assert!(!shown.durable_mutation);
    let plan = shown.planned_application.unwrap();
    assert_eq!(plan.status, "ready", "{:?}", plan.errors);
    assert!(
        plan.planned_evidence_attachments.is_empty(),
        "owned sources must not be advertised as new attachments"
    );
    assert_eq!(
        plan.shared_evidence_spans.len(),
        candidates[1].source_ids.len()
    );
    assert!(
        plan.shared_evidence_spans
            .iter()
            .all(|span| span.owner_memory_id == first)
    );
    let link = plan.planned_session_arc_link.unwrap();
    assert!(link.src_memory_id == first || link.dst_memory_id == first);
    assert!(!link.directed);
    assert_eq!(link.relation, "related");
    assert_eq!(
        link.arc_id,
        candidates[1].session_arc.as_ref().unwrap().arc_id
    );
    assert!(
        connection
            .list_memory_links_for_memory(&first, None)
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    assert_eq!(
        connection
            .list_audit_entries(Some(&fixture.workspace_id), None)
            .map_err(|error| error.to_string())?
            .len(),
        audits
    );
    let preview = apply_arc(&fixture, &candidates[1].candidate_id, true)?;
    assert!(
        preview
            .application
            .changes
            .iter()
            .any(|change| change.field == "sessionArcLinkId" && change.after.is_some())
    );
    assert!(!preview.mutation.persisted);
    Ok(())
}

const LATER_FAILURE: &str =
    "Failure arc: rewriting evidence ownership would violate the source-provenance policy.";
const LATER_REPAIR: &str =
    "Fix: preserve the original evidence owner and audit explicit learning decisions.";

#[test]
fn multiple_inline_episodes_keep_distinct_reciprocal_ids_and_exact_sources() {
    let session = synthetic_stored_session();
    let body = format!("{INLINE_ARC}\n{LATER_FAILURE}\n{LATER_REPAIR}");
    let span = synthetic_span("ev_multiple_arcs", None, &body);
    let candidates = super::super::build_session_arc_candidates(
        &session.workspace_id,
        &session,
        std::slice::from_ref(&span),
        0.0,
    );
    assert_eq!(
        candidates.len(),
        4,
        "both episodes must reach the review path"
    );
    let ids: std::collections::BTreeSet<_> = candidates
        .iter()
        .map(|candidate| candidate.candidate_id.as_str())
        .collect();
    assert_eq!(ids.len(), 4, "one episode must not overwrite another");
    for candidate in &candidates {
        assert_eq!(candidate.source_ids, std::slice::from_ref(&span.id));
        let arc = candidate.session_arc.as_ref().unwrap();
        for source in [&arc.failure_span, &arc.resolution_span] {
            assert_eq!(source.evidence_span_id, span.id);
            assert_eq!(source.content_hash, span.content_hash);
            assert_eq!(source.start_line, span.start_line);
            assert_eq!(source.end_line, span.end_line);
            assert_eq!(source.provenance_uri, span.canonical_provenance_uri());
        }
        let peer = candidates
            .iter()
            .find(|peer| peer.candidate_id == arc.linked_candidate_id)
            .expect("reciprocal proposal");
        let peer_arc = peer.session_arc.as_ref().unwrap();
        assert_eq!(peer_arc.linked_candidate_id, candidate.candidate_id);
        assert_ne!(peer_arc.role, arc.role);
        assert_eq!(peer_arc.failure_span, arc.failure_span);
        assert_eq!(peer_arc.resolution_span, arc.resolution_span);
        if arc
            .failure_span
            .excerpt
            .contains("rewriting evidence ownership")
        {
            assert_eq!(arc.failure_span.excerpt, LATER_FAILURE);
            assert_eq!(arc.resolution_span.excerpt, LATER_REPAIR);
            assert!(!candidate.proposed_content.contains("no-loop-takeover"));
        } else {
            assert!(arc.failure_span.excerpt.contains("no-loop-takeover"));
            assert!(
                !candidate
                    .proposed_content
                    .contains("rewriting evidence ownership")
            );
        }
    }
    for limit in 0..7 {
        let mut limited = candidates.clone();
        session_arc::limit_complete_pairs(&mut limited, limit);
        assert_eq!(limited.len(), limit.min(4) / 2 * 2);
        for candidate in &limited {
            let peer_id = &candidate.session_arc.as_ref().unwrap().linked_candidate_id;
            assert!(limited.iter().any(|peer| &peer.candidate_id == peer_id));
        }
    }
}

#[test]
fn repeated_inline_episodes_deduplicate_complete_pairs_without_changing_first_ids() {
    let session = synthetic_stored_session();
    let span = synthetic_span("ev_repeated_arcs", None, INLINE_ARC);
    let original = session_arc::inline_candidates(
        &session.workspace_id,
        &session,
        std::slice::from_ref(&span),
    );
    let mut repeated = span.clone();
    // Keep source identity fixed to isolate extraction from evidence revision.
    repeated.excerpt = format!("{INLINE_ARC}\n").repeat(8);
    let actual = session_arc::inline_candidates(
        &session.workspace_id,
        &session,
        &[repeated.clone(), repeated],
    );
    assert_eq!(actual, original);
    let mut extended = span;
    extended.excerpt = format!("{INLINE_ARC}\n{LATER_FAILURE}\n{LATER_REPAIR}");
    let actual = session_arc::inline_candidates(&session.workspace_id, &session, &[extended]);
    assert_eq!(actual.len(), 4);
    assert_eq!(&actual[..2], original.as_slice());
}

#[test]
fn later_inline_pair_persists_reconstructs_and_applies_without_accepting_earlier_pair() -> TestResult
{
    let fixture = review_session_fixture()?;
    let session = SessionId::from_uuid(uuid::Uuid::from_u128(9810)).to_string();
    let source_id = evidence_id(9811);
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    connection
        .insert_session(
            &session,
            &session_input(&fixture.workspace_id, "multiple-session-arcs"),
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_evidence_span(
            &source_id,
            &evidence_span_input(
                &fixture.workspace_id,
                &session,
                None,
                "multiple-arc-span",
                60,
                &format!("{INLINE_ARC}\n{LATER_FAILURE}\n{LATER_REPAIR}"),
            ),
        )
        .map_err(|error| error.to_string())?;
    let before = connection
        .list_memories(&fixture.workspace_id, None, false)
        .map_err(|error| error.to_string())?
        .len();
    connection.close().map_err(|error| error.to_string())?;
    let report = review_session_proposals(&ReviewSessionOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.database_path),
        session_id: Some(&session),
        propose: true,
        dry_run: false,
        min_confidence: 0.8,
        limit: 4,
    })
    .map_err(|error| error.message())?;
    assert_eq!(report.candidate_count, 4, "{:?}", report.candidates);
    assert!(
        report
            .candidates
            .iter()
            .all(|candidate| candidate.persisted)
    );
    let connection =
        DbConnection::open_file(&fixture.database_path).map_err(|error| error.to_string())?;
    for candidate in &report.candidates {
        let stored = connection
            .get_curation_candidate(&fixture.workspace_id, &candidate.candidate_id)
            .map_err(|error| error.to_string())?
            .expect("persisted proposal");
        assert_eq!(stored.status, "pending");
        // This re-reads the real evidence and verifies the complete source
        // package, identity and reciprocal metadata, even for later episodes.
        let peer = session_arc::applied_peer(&connection, &stored)
            .map_err(|issue| format!("{}: {}", issue.code, issue.message))?;
        assert!(peer.is_none(), "proposal is not an accepted lesson");
    }
    assert_eq!(
        connection
            .list_memories(&fixture.workspace_id, None, false)
            .map_err(|error| error.to_string())?
            .len(),
        before
    );
    let selected: Vec<_> = report
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.session_arc.as_ref().is_some_and(|arc| {
                arc.failure_span
                    .excerpt
                    .contains("rewriting evidence ownership")
            })
        })
        .collect();
    assert_eq!(selected.len(), 2);
    let mut created = Vec::new();
    for candidate in &selected {
        validate_arc(&fixture, &candidate.candidate_id)?;
        let applied = apply_arc(&fixture, &candidate.candidate_id, false)?;
        assert_eq!(
            applied.application.status, "applied",
            "{:?}",
            applied.application.errors
        );
        created.push(applied.application.created_memory_id.unwrap());
    }
    for candidate in &report.candidates {
        let stored = connection
            .get_curation_candidate(&fixture.workspace_id, &candidate.candidate_id)
            .map_err(|error| error.to_string())?
            .unwrap();
        let selected = selected
            .iter()
            .any(|chosen| chosen.candidate_id == candidate.candidate_id);
        assert_eq!(stored.status, if selected { "applied" } else { "pending" });
    }
    let source = connection
        .get_evidence_span(&source_id)
        .map_err(|error| error.to_string())?
        .unwrap();
    assert_eq!(source.memory_id.as_deref(), Some(created[0].as_str()));
    let links = connection
        .list_memory_links_for_memory(&created[0], Some(MemoryLinkRelation::Related))
        .map_err(|error| error.to_string())?;
    assert_eq!(links.len(), 1);
    let link = &links[0];
    assert!(!link.directed);
    for memory_id in &created {
        assert!(link.src_memory_id == *memory_id || link.dst_memory_id == *memory_id);
    }
    let details = link.metadata_json.as_deref().unwrap();
    for candidate in &selected {
        assert!(details.contains(&candidate.candidate_id));
    }
    let audits = connection
        .list_audit_by_target("memory_link", &link.id, None)
        .map_err(|error| error.to_string())?;
    assert_eq!(audits.len(), 1);
    let audit_count = connection
        .list_audit_entries(Some(&fixture.workspace_id), None)
        .map_err(|error| error.to_string())?
        .len();
    for candidate in selected {
        assert_eq!(
            apply_arc(&fixture, &candidate.candidate_id, false)?
                .application
                .status,
            "already_applied"
        );
    }
    assert_eq!(
        connection
            .list_memories(&fixture.workspace_id, None, false)
            .map_err(|error| error.to_string())?
            .len(),
        before + 2
    );
    assert_eq!(
        connection
            .list_audit_entries(Some(&fixture.workspace_id), None)
            .map_err(|error| error.to_string())?
            .len(),
        audit_count
    );
    Ok(())
}

fn sequence_span(id: &str, line: u32, excerpt: &str) -> StoredEvidenceSpan {
    let mut span = synthetic_span(id, None, excerpt);
    span.start_line = line;
    span.end_line = line;
    span.content_hash = super::super::content_hash_for_candidate(excerpt);
    span
}

fn sequence_candidates(spans: &[StoredEvidenceSpan]) -> Vec<ReviewSessionCandidate> {
    let session = synthetic_stored_session();
    super::super::build_session_arc_candidates(&session.workspace_id, &session, spans, 0.0)
}

fn sequence_endpoints(rows: &[ReviewSessionCandidate]) -> Vec<(String, String)> {
    rows.iter()
        .filter(|row| row.candidate_kind == REVIEW_CANDIDATE_KIND_SESSION_ARC_RULE)
        .map(|row| {
            let arc = row.session_arc.as_ref().expect("session arc");
            (
                arc.failure_span.evidence_span_id.clone(),
                arc.resolution_span.evidence_span_id.clone(),
            )
        })
        .collect()
}

#[test]
fn session_arc_sequence_mines_every_disjoint_episode_of_the_same_topic() {
    let spans = [
        sequence_span(
            "failure-a",
            1,
            "cargo test failed because the cache key was stale.",
        ),
        sequence_span("repair-a", 2, "Fixed the cache key and cargo test passed."),
        sequence_span(
            "failure-b",
            3,
            "cargo test failed because the fixture path was absent.",
        ),
        sequence_span(
            "repair-b",
            4,
            "Fixed the fixture path and cargo test passed.",
        ),
        sequence_span("extra-success", 5, "cargo test passed again."),
    ];
    let rows = sequence_candidates(&spans);
    assert_eq!(rows.len(), 4);
    assert_eq!(
        sequence_endpoints(&rows),
        [
            ("failure-a".into(), "repair-a".into()),
            ("failure-b".into(), "repair-b".into())
        ]
    );
    let prefix = sequence_candidates(&spans[..2]);
    assert_eq!(
        prefix,
        rows[..2],
        "appending an episode cannot change a completed lesson"
    );
    let mut reversed = spans.to_vec();
    reversed.reverse();
    assert_eq!(sequence_candidates(&reversed), rows);
    reversed.rotate_left(2);
    assert_eq!(sequence_candidates(&reversed), rows);
    for limit in 0..=5 {
        let mut bounded = rows.clone();
        session_arc::limit_complete_pairs(&mut bounded, limit);
        assert_eq!(bounded.len(), (limit / 2 * 2).min(4));
        for row in &bounded {
            let arc = row.session_arc.as_ref().unwrap();
            assert!(
                bounded
                    .iter()
                    .any(|peer| peer.candidate_id == arc.linked_candidate_id)
            );
        }
    }
}

#[test]
fn session_arc_sequence_keeps_interleaved_topics_separate() {
    let spans = [
        sequence_span(
            "format-failed",
            1,
            "cargo fmt failed on the generated source.",
        ),
        sequence_span("lint-failed", 2, "cargo clippy failed on an unused import."),
        sequence_span(
            "format-fixed",
            3,
            "cargo fmt passed after formatting the source.",
        ),
        sequence_span(
            "lint-fixed",
            4,
            "cargo clippy passed after removing the unused import.",
        ),
    ];
    assert_eq!(
        sequence_endpoints(&sequence_candidates(&spans)),
        [
            ("format-failed".into(), "format-fixed".into()),
            ("lint-failed".into(), "lint-fixed".into()),
        ]
    );
}

#[test]
fn session_arc_sequence_uses_the_nearest_failure_and_consumes_it_once() {
    let spans = [
        sequence_span(
            "obsolete-failure",
            1,
            "cargo test failed with the old implementation.",
        ),
        sequence_span(
            "current-failure",
            2,
            "cargo test failed with a missing fixture.",
        ),
        sequence_span("repair", 3, "cargo test passed with the fixture restored."),
        sequence_span("later-success", 4, "cargo test passed again."),
    ];
    assert_eq!(
        sequence_endpoints(&sequence_candidates(&spans)),
        [("current-failure".into(), "repair".into())]
    );
}

#[test]
fn session_arc_sequence_mines_all_explicit_cross_topic_declarations() {
    let spans = [
        sequence_span(
            "policy-failure",
            1,
            "Failure arc: silent storage violated the no-loop-takeover policy.",
        ),
        sequence_span(
            "policy-fix",
            2,
            "Fix: require explicit accept/reject commands and audit every capture.",
        ),
        sequence_span(
            "ownership-failure",
            3,
            "Failure arc: rewritten ownership broke immutable provenance.",
        ),
        sequence_span(
            "ownership-fix",
            4,
            "Fix: preserve the original evidence owner and audit explicit decisions.",
        ),
    ];
    assert_eq!(
        sequence_endpoints(&sequence_candidates(&spans)),
        [
            ("policy-failure".into(), "policy-fix".into()),
            ("ownership-failure".into(), "ownership-fix".into()),
        ]
    );
}

#[test]
fn session_arc_sequence_decodes_bodies_without_learning_from_json_metadata() {
    let failure = "cargo test failed because 資料 cache identity was stale.";
    let repair = "cargo test passed after the 資料 cache identity was repaired.";
    let first =
        serde_json::json!({"type":"assistant", "message":{"role":"assistant","content":failure},
        "metadata":{"topic":"rustfmt","content":"metadata-sentinel"}})
        .to_string();
    let second = serde_json::json!({"type":"response_item", "payload":{"type":"message","role":"assistant", "content":[{"type":"output_text","text":repair}]},
        "metadata":{"topic":"clippy","content":"metadata-sentinel"}}).to_string();
    let spans = [
        sequence_span("source-failure", 4, &first),
        sequence_span("source-repair", 9, &second),
    ];
    let rows = sequence_candidates(&spans);
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.topic_key, "testing");
        assert!(row.proposed_content.contains(failure));
        assert!(row.proposed_content.contains(repair));
        assert!(!row.proposed_content.contains("metadata-sentinel"));
        let arc = row.session_arc.as_ref().unwrap();
        for (locator, source) in [
            (&arc.failure_span, &spans[0]),
            (&arc.resolution_span, &spans[1]),
        ] {
            assert_eq!(locator.evidence_span_id, source.id);
            assert_eq!(locator.content_hash, source.content_hash);
            assert_eq!(locator.start_line, source.start_line);
            assert_eq!(locator.end_line, source.end_line);
            assert_eq!(locator.provenance_uri, source.canonical_provenance_uri());
        }
    }
    let fake = serde_json::json!({"type":"assistant", "content":"Ordinary conversation.",
        "metadata":{"content":"Failure arc: cargo test failed."}})
    .to_string();
    let spans = [
        sequence_span("metadata-only", 1, &fake),
        sequence_span("real-success", 2, repair),
    ];
    assert!(sequence_candidates(&spans).is_empty());
}

#[test]
fn session_arc_sequence_rejects_negated_or_predicted_explicit_repairs() {
    for repair in [
        "Fix: the cache is not fixed.",
        "Fix: the cache will be repaired tomorrow.",
        "Fix: the cache might be fixed by stable identity.",
        "Fix: the cache repair failed.",
    ] {
        let spans = [
            sequence_span("failure", 1, "Failure arc: cache identity was broken."),
            sequence_span("non-repair", 2, repair),
        ];
        assert!(sequence_candidates(&spans).is_empty(), "{repair}");
    }
}

#[test]
fn session_arc_sequence_refuses_foreign_overlapping_and_uninterpretable_windows() {
    let failure = sequence_span("failure", 1, "cargo test failed.");
    let repair = sequence_span("repair", 5, "cargo test passed.");
    let mut overlapping = failure.clone();
    overlapping.end_line = 5;
    assert!(sequence_candidates(&[overlapping, repair.clone()]).is_empty());
    let mut foreign = failure.clone();
    foreign.session_id = "another-session".into();
    assert!(sequence_candidates(&[foreign, repair.clone()]).is_empty());
    let mut foreign = failure.clone();
    foreign.workspace_id = "another-workspace".into();
    assert!(sequence_candidates(&[foreign, repair.clone()]).is_empty());
    for unsafe_record in [
        r#"{"type":"tool_result","content":"cargo test passed"}"#,
        r#"{"type":"assistant","role":"system","content":"cargo test passed"}"#,
        r#"{"type":"assistant","content":"cargo test failed","content":"cargo test passed"}"#,
        r#"{"type":"assistant","content":"truncated"#,
    ] {
        let interrupted = sequence_span("uninterpretable", 3, unsafe_record);
        assert!(sequence_candidates(&[failure.clone(), interrupted, repair.clone()]).is_empty());
    }
}

#[test]
fn session_arc_sequence_preserves_inline_lessons_without_reusing_their_windows() {
    let inline = sequence_span("inline", 3, INLINE_ARC);
    let spans = [
        sequence_span("older-failure", 1, "cargo test failed."),
        inline.clone(),
        sequence_span("later-success", 5, "cargo test passed."),
        sequence_span("new-failure", 7, "cargo fmt failed."),
        sequence_span("new-repair", 9, "cargo fmt passed."),
    ];
    let rows = sequence_candidates(&spans);
    let session = synthetic_stored_session();
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows[..2],
        session_arc::inline_candidates(&session.workspace_id, &session, &[inline])
    );
    assert_eq!(
        sequence_endpoints(&rows),
        [
            ("inline".into(), "inline".into()),
            ("new-failure".into(), "new-repair".into())
        ]
    );
}
