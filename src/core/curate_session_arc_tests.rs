use super::*;
use crate::core::curate::session_arc;

const INLINE_ARC: &str = "Failure arc: storing silently would violate the no-loop-takeover policy.\nFix: require accept/reject commands and audit every accepted capture.";

#[test]
fn session_arc_within_one_window_retains_exact_source_identity() {
    let session = synthetic_stored_session();
    let span = synthetic_span("ev_arc_window", None, INLINE_ARC);
    let candidates = super::super::build_session_arc_candidates(
        &session.workspace_id,
        &session,
        &[span.clone()],
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
            &[span.clone()],
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
