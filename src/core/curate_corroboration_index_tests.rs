//! Exact selection and work bounds for the connected corroboration join.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;

fn session() -> StoredSession {
    StoredSession {
        id: "ses_resource0000000000000000000".into(),
        workspace_id: "wsp_resource0000000000000000000".into(),
        cass_session_id: "ordinary-repair-prose".into(),
        source_path: None,
        agent_name: None,
        model: None,
        started_at: None,
        ended_at: None,
        message_count: 0,
        token_count: None,
        content_hash: format!("blake3:{}", blake3::hash(b"resource-session").to_hex()),
        metadata_json: None,
        imported_at: "2026-09-22T00:00:00Z".into(),
        updated_at: "2026-09-22T00:00:00Z".into(),
    }
}

fn span(id: &str, line: u32, text: &str) -> StoredEvidenceSpan {
    let hash = format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex());
    StoredEvidenceSpan {
        id: id.into(),
        workspace_id: session().workspace_id,
        session_id: session().id,
        memory_id: None,
        cass_span_id: format!("cass-{id}"),
        span_kind: "message".into(),
        start_line: line,
        end_line: line,
        start_byte: None,
        end_byte: None,
        role: Some("assistant".into()),
        excerpt: text.into(),
        reader_projection: None,
        content_hash: hash.clone(),
        metadata_json: None,
        producer_kind: "cass_import".into(),
        screening_version: 1,
        secret_redaction_status: "clean".into(),
        redaction_classes_json: "[]".into(),
        instruction_risk: "none".into(),
        search_eligibility: "admitted".into(),
        pack_eligibility: "admitted".into(),
        canonical_provenance_revision: 1,
        canonical_excerpt_hash: Some(hash),
        security_policy_epoch: 1,
        upstream_ref_hash: Some(format!(
            "blake3:{}",
            blake3::hash(format!("cass-{id}").as_bytes()).to_hex()
        )),
        created_at: "2026-09-22T00:00:00Z".into(),
        updated_at: "2026-09-22T00:00:00Z".into(),
    }
}

fn template() -> ReviewSessionCandidate {
    let session = session();
    build_session_arc_candidate_pair(
        &session.workspace_id,
        &session,
        "testing",
        &span("failure", 1, "cargo test src/api.rs failed."),
        &span("repair", 2, "Fixed src/api.rs by restoring the guard."),
    )
    .into_iter()
    .next()
    .expect("complete source-backed pair")
}

fn observation(id: &str) -> SupportingSession {
    let source = source_reference(&span(id, 1, "Complete evidence."));
    SupportingSession {
        session_id: id.into(),
        upstream_hash: format!("upstream-{id}"),
        source_path_hash: Some(format!("path-{id}")),
        payload_hash: format!("payload-{id}"),
        candidate_id: id.into(),
        sources: vec![source.clone()],
        context_sources: vec![source],
    }
}

fn candidate(
    base: &ReviewSessionCandidate,
    id: &str,
    kind: &str,
    key: &str,
) -> ReviewSessionCandidate {
    let mut candidate = base.clone();
    candidate.candidate_id = id.into();
    candidate.candidate_kind = kind.into();
    let mut proof = ReviewCorroboration::pending(key.into());
    proof.sessions.push(observation(&format!("anchor-{id}")));
    candidate.corroboration = Some(proof);
    candidate
}

fn signature(candidate: &ReviewSessionCandidate) -> Option<String> {
    Some(candidate.corroboration.as_ref()?.signature.clone())
}

type Observations = BTreeMap<String, Option<SupportingSession>>;

fn observed(
    candidate: &ReviewSessionCandidate,
    observations: &Observations,
) -> Option<SupportingSession> {
    observations.get(&candidate.candidate_id).cloned().flatten()
}

/// The original anchor-major loop is retained as an independent test oracle.
fn legacy(
    candidates: &mut [ReviewSessionCandidate],
    others: &[ReviewSessionCandidate],
    observations: &Observations,
) {
    for candidate in candidates.iter_mut() {
        let Some(proof) = candidate.corroboration.as_mut() else {
            continue;
        };
        if proof.sessions.is_empty() || proof.sessions.len() >= MAX_SUPPORT {
            continue;
        }
        for other in others {
            if other.candidate_kind != candidate.candidate_kind
                || signature(other).as_ref() != Some(&proof.signature)
            {
                continue;
            }
            if let Some(additional) = observed(other, observations)
                && independent(&additional, &proof.sessions)
            {
                proof.sessions.push(additional);
                break;
            }
        }
    }
}

fn assert_equivalent(
    candidates: &[ReviewSessionCandidate],
    others: &[ReviewSessionCandidate],
    observations: &Observations,
) {
    let mut expected = candidates.to_vec();
    legacy(&mut expected, others, observations);
    let mut actual = candidates.to_vec();
    corroborate_candidates(&mut actual, others, signature, |other| {
        Ok(observed(other, observations))
    })
    .expect("read-only observations");
    assert_eq!(
        actual, expected,
        "complete candidates, not just support counts"
    );
}

#[test]
fn indexed_join_preserves_first_valid_support_and_every_provenance_field() {
    let base = template();
    let mut anchor = candidate(&base, "anchor", "rule", "exact");
    let mut aliases = observation("alias");
    aliases.payload_hash = anchor.corroboration.as_ref().unwrap().sessions[0]
        .payload_hash
        .clone();
    let others: Vec<_> = ["alias", "rejected", "first", "later"]
        .into_iter()
        .map(|id| candidate(&base, id, "rule", "exact"))
        .collect();
    let observations = BTreeMap::from([
        ("alias".into(), Some(aliases)),
        ("rejected".into(), None),
        ("first".into(), Some(observation("first"))),
        ("later".into(), Some(observation("later"))),
    ]);
    assert_equivalent(std::slice::from_ref(&anchor), &others, &observations);
    let mut read = Vec::new();
    corroborate_candidates(
        std::slice::from_mut(&mut anchor),
        &others,
        signature,
        |other| {
            read.push(other.candidate_id.clone());
            Ok(observed(other, &observations))
        },
    )
    .unwrap();
    assert_eq!(read, ["alias", "rejected", "first"]);
    assert_eq!(
        anchor.corroboration.unwrap().sessions[1],
        observation("first")
    );
}

#[test]
fn one_session_can_supply_at_most_one_increment_per_anchor() {
    let base = template();
    let anchors: Vec<_> = (0..128)
        .map(|index| candidate(&base, &format!("anchor-{index}"), "rule", "shared"))
        .collect();
    let others: Vec<_> = (0..128)
        .map(|index| candidate(&base, &format!("other-{index}"), "rule", "shared"))
        .collect();
    let before = anchors.clone();
    let mut actual = anchors;
    let mut signatures = 0;
    let mut reads = 0;
    corroborate_candidates(
        &mut actual,
        &others,
        |other| {
            signatures += 1;
            signature(other)
        },
        |other| {
            reads += 1;
            Ok(Some(observation(&other.candidate_id)))
        },
    )
    .unwrap();
    assert_eq!((signatures, reads), (1, 1));
    for (old, new) in before.iter().zip(&actual) {
        let proof = new.corroboration.as_ref().unwrap();
        assert_eq!(proof.sessions.len(), 2);
        assert_eq!(
            proof.sessions[0],
            old.corroboration.as_ref().unwrap().sessions[0]
        );
        assert_eq!(proof.sessions[1], observation("other-0"));
        assert_eq!(new.source_ids, old.source_ids);
        assert_eq!(new.session_arc, old.session_arc);
    }
}

#[test]
fn distinct_lesson_join_signs_each_other_once_not_once_per_anchor() {
    let base = template();
    let mut anchors: Vec<_> = (0..256)
        .map(|index| {
            candidate(
                &base,
                &format!("anchor-{index}"),
                "rule",
                &format!("key-{index}"),
            )
        })
        .collect();
    let others: Vec<_> = (0..256)
        .rev()
        .map(|index| {
            candidate(
                &base,
                &format!("other-{index}"),
                "rule",
                &format!("key-{index}"),
            )
        })
        .collect();
    let mut signatures = 0;
    let mut reads = 0;
    corroborate_candidates(
        &mut anchors,
        &others,
        |other| {
            signatures += 1;
            signature(other)
        },
        |other| {
            reads += 1;
            Ok(Some(observation(&other.candidate_id)))
        },
    )
    .unwrap();
    assert_eq!((signatures, reads), (256, 256));
    for (index, anchor) in anchors.iter().enumerate() {
        assert_eq!(
            anchor.corroboration.as_ref().unwrap().sessions[1],
            observation(&format!("other-{index}"))
        );
    }
}

#[test]
fn irrelevant_roles_signatures_and_saturated_anchors_do_not_read_proofs() {
    let base = template();
    let mut anchors = vec![candidate(&base, "active", "rule", "exact")];
    let mut unverified = candidate(&base, "unverified", "rule", "empty");
    unverified.corroboration.as_mut().unwrap().sessions.clear();
    anchors.push(unverified);
    let mut saturated = candidate(&base, "saturated", "rule", "full");
    saturated
        .corroboration
        .as_mut()
        .unwrap()
        .sessions
        .extend([observation("one"), observation("two")]);
    anchors.push(saturated);
    let mut proofless = candidate(&base, "proofless", "rule", "absent");
    proofless.corroboration = None;
    anchors.push(proofless);
    let before = anchors.clone();
    let others = vec![
        candidate(&base, "wrong-role", "anti_pattern", "exact"),
        candidate(&base, "wrong-case", "rule", "Exact"),
        candidate(&base, "empty", "rule", "empty"),
        candidate(&base, "full", "rule", "full"),
        candidate(&base, "absent", "rule", "absent"),
    ];
    corroborate_candidates(&mut anchors, &others, signature, |_| {
        Err(domain("must not read"))
    })
    .expect("irrelevant proposals never reach storage");
    assert_eq!(anchors, before);
}

#[test]
fn failed_matching_read_aborts_instead_of_falling_back_to_another_supporter() {
    let base = template();
    let mut anchors = vec![candidate(&base, "anchor", "rule", "exact")];
    let others = vec![
        candidate(&base, "error", "rule", "exact"),
        candidate(&base, "later", "rule", "exact"),
    ];
    let mut reads = 0;
    let result = corroborate_candidates(&mut anchors, &others, signature, |_| {
        reads += 1;
        Err(domain("read failed"))
    });
    assert!(result.is_err());
    assert_eq!(reads, 1);
    assert_eq!(anchors[0].corroboration.as_ref().unwrap().sessions.len(), 1);
}

#[test]
fn exact_full_record_signatures_keep_qualifications_and_roles_out_of_the_same_bucket() {
    let base = template();
    let body = format!(
        "Always run cargo test --locked. {} Only apply to protocol 17.",
        "Operational qualification. ".repeat(15)
    );
    let source = span("source", 1, &body);
    let mut anchor = candidate(&base, "anchor", "rule", "seed");
    anchor.source_ids = vec![source.id.clone()];
    let expected = full_signature(&anchor, std::slice::from_ref(&source)).unwrap();
    anchor.corroboration.as_mut().unwrap().signature = expected;
    let other = candidate(&base, "other", "rule", "seed");
    let mut other = ReviewSessionCandidate {
        source_ids: vec![source.id.clone()],
        ..other
    };
    for changed in [
        body.replace("protocol 17", "protocol 18"),
        body.replace("Only apply", "Never apply"),
    ] {
        let changed = span("source", 1, &changed);
        let mut targets = vec![anchor.clone()];
        corroborate_candidates(
            &mut targets,
            std::slice::from_ref(&other),
            |row| full_signature(row, std::slice::from_ref(&changed)),
            |_| Err(domain("different full source")),
        )
        .unwrap();
        assert_eq!(targets, [anchor.clone()]);
    }
    let mut changed_role = source.clone();
    changed_role.role = Some("user".into());
    let mut targets = vec![anchor.clone()];
    corroborate_candidates(
        &mut targets,
        std::slice::from_ref(&other),
        |row| full_signature(row, std::slice::from_ref(&changed_role)),
        |_| Err(domain("different role")),
    )
    .unwrap();
    assert_eq!(targets, [anchor]);
    other.corroboration = None;
    assert!(full_signature(&other, &[source]).is_none());
}

#[test]
fn indexed_selection_matches_legacy_across_kinds_aliases_rejections_and_source_orders() {
    let base = template();
    // Deterministic enumeration exercises every combination, not a random seed
    // whose rare coverage could disappear when this fixture changes.
    for scenario in 0_u32..256 {
        let mut anchors = vec![
            candidate(&base, "a", "rule", "same"),
            candidate(&base, "b", "rule", "same"),
            candidate(&base, "c", "anti_pattern", "same"),
            candidate(&base, "d", "rule", "different"),
        ];
        let mut others: Vec<_> = (0..4)
            .map(|index| {
                candidate(
                    &base,
                    &format!("other-{index}"),
                    if scenario & (1 << index) == 0 {
                        "rule"
                    } else {
                        "anti_pattern"
                    },
                    if index == 3 { "different" } else { "same" },
                )
            })
            .collect();
        let observations: Observations = others
            .iter()
            .enumerate()
            .map(|(index, other)| {
                (
                    other.candidate_id.clone(),
                    (scenario & (16 << index) == 0).then(|| observation(&other.candidate_id)),
                )
            })
            .collect();
        for alias in ["session", "upstream", "payload", "path"] {
            let mut prior = observation("prior");
            let other = observation("other-0");
            match alias {
                "session" => prior.session_id = other.session_id,
                "upstream" => prior.upstream_hash = other.upstream_hash,
                "payload" => prior.payload_hash = other.payload_hash,
                _ => prior.source_path_hash = other.source_path_hash,
            }
            anchors[0].corroboration.as_mut().unwrap().sessions = vec![prior];
            for _ in 0..others.len() {
                assert_equivalent(&anchors, &others, &observations);
                others.reverse();
                assert_equivalent(&anchors, &others, &observations);
                others.reverse();
                others.rotate_left(1);
            }
        }
    }
}

/// Pre-cache source construction, including historical prefix semantics.
fn legacy_support(
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
    candidate: &ReviewSessionCandidate,
    recorded: Option<&SupportingSession>,
) -> Option<SupportingSession> {
    let mut sources = Vec::new();
    for id in &candidate.source_ids {
        let span = spans.iter().find(|span| span.id == *id)?;
        if !span.is_search_admitted_for_session(&session.workspace_id, session)
            || review_learning_text(span).is_none()
        {
            return None;
        }
        sources.push(source_reference(span));
    }
    sources.sort_by(|left, right| left.evidence_span_id.cmp(&right.evidence_span_id));
    sources.dedup();
    if sources.is_empty() {
        return None;
    }
    let mut payload = vec!["complete_session".to_owned()];
    let mut seen_events = BTreeSet::new();
    let mut context_sources = Vec::new();
    let last_line = recorded
        .map(|recorded| {
            recorded
                .context_sources
                .iter()
                .map(|source| source.end_line)
                .max()
        })
        .unwrap_or(Some(u32::MAX))?;
    for span in spans.iter().filter(|span| span.start_line <= last_line) {
        context_sources.push(source_reference(span));
        let text = review_learning_text(span).unwrap_or(Cow::Borrowed(&span.excerpt));
        let event = digest([
            span.span_kind.as_str(),
            span.role.as_deref().unwrap_or(""),
            text.as_ref(),
        ]);
        if seen_events.insert(event.clone()) {
            payload.push(event);
        }
    }
    if recorded.is_some_and(|recorded| recorded.context_sources != context_sources) {
        return None;
    }
    Some(SupportingSession {
        session_id: session.id.clone(),
        upstream_hash: digest(["upstream", session.cass_session_id.as_str()]),
        source_path_hash: session
            .source_path
            .as_deref()
            .map(|path| digest(["source_path", path])),
        payload_hash: digest(&payload),
        candidate_id: candidate.candidate_id.clone(),
        sources,
        context_sources,
    })
}

fn legacy_reconstruct(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
    expected: &SupportingSession,
) -> Option<ReviewSessionCandidate> {
    let mut sources = expected
        .sources
        .iter()
        .map(|source| spans.iter().find(|span| span.id == source.evidence_span_id))
        .collect::<Option<Vec<_>>>()?;
    sources.sort_by(|left, right| session_arc_span_order(left, right));
    let topic = review_topic_key(&review_learning_text(sources.first()?)?);
    if topic != "noise"
        && sources.iter().all(|source| {
            review_learning_text(source).is_some_and(|text| review_topic_key(&text) == topic)
        })
        && let Some(candidate) = build_bootstrap_candidate(workspace_id, session, &topic, &sources)
        && candidate.candidate_id == expected.candidate_id
    {
        return Some(candidate);
    }
    build_session_arc_candidates(workspace_id, session, spans, 0.0)
        .into_iter()
        .find(|candidate| candidate.candidate_id == expected.candidate_id)
}

#[test]
fn one_snapshot_reuses_arc_mining_and_complete_context_for_many_lessons() {
    let session = session();
    let mut spans = Vec::new();
    for index in 0..64 {
        spans.push(span(
            &format!("failure-{index}"),
            index * 2 + 1,
            &format!("Failure arc: cargo test src/file_{index}.rs failed."),
        ));
        spans.push(span(
            &format!("repair-{index}"),
            index * 2 + 2,
            &format!("Fix: cargo test src/file_{index}.rs passed after restoring the guard."),
        ));
    }
    let before = spans.clone();
    let rows = build_session_arc_candidates(&session.workspace_id, &session, &spans, 0.0);
    assert_eq!(rows.len(), 128, "all independent episodes, both roles");
    let evidence = SessionEvidence::new(&session.workspace_id, &session, &spans);
    for row in rows.iter().chain(rows.iter().rev()) {
        let expected = legacy_support(&session, &spans, row, None).expect("admitted source");
        let actual = evidence.support(row).expect("cached support");
        assert_eq!(actual, expected);
        assert_eq!(
            serde_json::to_string(&actual).unwrap(),
            serde_json::to_string(&expected).unwrap()
        );
        assert_eq!(evidence.reconstruct(&actual), Some(row.clone()));
    }
    assert_eq!(evidence.arc_builds.get(), 1);
    assert_eq!(evidence.context_builds.get(), 1);
    assert_eq!(evidence.arcs.get().unwrap().len(), rows.len());
    assert_eq!(spans, before, "neither cache mutates or attaches evidence");
}

#[test]
fn recorded_bootstrap_subset_does_not_expand_to_current_topic_group_or_mine_arcs() {
    let session = session();
    let spans = [
        span(
            "lesson",
            1,
            "Always run cargo test --locked after changing src/support.rs.",
        ),
        span(
            "later",
            3,
            "Always run cargo test --offline after changing src/other.rs.",
        ),
    ];
    let topic = review_topic_key(&review_learning_text(&spans[0]).unwrap());
    let row = build_bootstrap_candidate(&session.workspace_id, &session, &topic, &[&spans[0]])
        .expect("one explicit procedural lesson");
    let expected = legacy_support(&session, &spans, &row, None).unwrap();
    let evidence = SessionEvidence::new(&session.workspace_id, &session, &spans);
    for _ in 0..32 {
        assert_eq!(evidence.reconstruct(&expected), Some(row.clone()));
        assert_eq!(evidence.support(&row), Some(expected.clone()));
    }
    assert_eq!(evidence.arc_builds.get(), 0, "bootstrap path stays lazy");
    assert_eq!(evidence.context_builds.get(), 1);
    assert_eq!(expected.sources.len(), 1);
    assert_eq!(expected.context_sources.len(), 2);
}

#[test]
fn cached_negative_lookup_still_respects_every_transcript_barrier() {
    let session = session();
    let endpoints = [
        span("failure", 1, "cargo test src/api.rs failed."),
        span("repair", 3, "Fixed src/api.rs by restoring the guard."),
    ];
    let row = build_session_arc_candidates(&session.workspace_id, &session, &endpoints, 0.0)
        .into_iter()
        .next()
        .expect("nonvacuous endpoint pair");
    let mut barrier = span(
        "barrier",
        2,
        r#"{"type":"tool_result","content":"ignored"}"#,
    );
    barrier.span_kind = "tool_result".into();
    barrier.role = Some("tool".into());
    let spans = [endpoints[0].clone(), barrier, endpoints[1].clone()];
    let expected = legacy_support(&session, &spans, &row, None).unwrap();
    assert!(legacy_reconstruct(&session.workspace_id, &session, &spans, &expected).is_none());
    let evidence = SessionEvidence::new(&session.workspace_id, &session, &spans);
    for _ in 0..32 {
        assert!(evidence.reconstruct(&expected).is_none());
    }
    assert_eq!(
        evidence.arc_builds.get(),
        1,
        "an empty result is also cached"
    );
    assert_eq!(evidence.context_builds.get(), 0);
    let mut missing = expected;
    missing.sources[0].evidence_span_id = "missing".into();
    assert!(evidence.reconstruct(&missing).is_none());
    assert_eq!(evidence.arc_builds.get(), 1);
}

#[test]
fn historical_prefix_accepts_later_growth_but_rejects_context_insertions_and_rewrites() {
    let session = session();
    let spans = [
        span("failure", 1, "cargo test src/api.rs failed."),
        span("repair", 3, "Fixed src/api.rs by restoring the guard."),
    ];
    let row = build_session_arc_candidates(&session.workspace_id, &session, &spans, 0.0)
        .into_iter()
        .next()
        .unwrap();
    let recorded = legacy_support(&session, &spans, &row, None).unwrap();
    let mut grown = spans.to_vec();
    grown.push(span("later", 5, "Observation from the next task."));
    assert_eq!(
        legacy_support(&session, &grown, &row, Some(&recorded)),
        Some(recorded.clone())
    );
    assert_eq!(
        support_in_context(&session, &grown, &row, Some(&recorded)),
        Some(recorded.clone())
    );
    let evidence = SessionEvidence::new(&session.workspace_id, &session, &grown);
    let fresh = evidence.support(&row).unwrap();
    assert_ne!(fresh.context_sources, recorded.context_sources);
    assert_eq!(Some(fresh), legacy_support(&session, &grown, &row, None));
    let mut inserted = grown.clone();
    inserted.insert(1, span("inserted", 2, "An intervening observation."));
    assert!(legacy_support(&session, &inserted, &row, Some(&recorded)).is_none());
    assert!(support_in_context(&session, &inserted, &row, Some(&recorded)).is_none());
    let mut rewritten = grown.clone();
    rewritten[0].metadata_json = Some(r#"{"qualification":"changed"}"#.into());
    assert!(support_in_context(&session, &rewritten, &row, Some(&recorded)).is_none());
    assert!(legacy_support(&session, &rewritten, &row, Some(&recorded)).is_none());
    let changed = SessionEvidence::new(&session.workspace_id, &session, &rewritten);
    assert_ne!(
        changed.support(&row),
        evidence.support(&row),
        "no cross-snapshot cache"
    );
}

#[test]
fn source_index_preserves_first_id_and_checks_admission_before_context_hashing() {
    let session = session();
    let mut row = template();
    row.source_ids = vec!["source".into(), "source".into()];
    let clean = span("source", 2, "Always run cargo test --locked.");
    let mut blocked = clean.clone();
    blocked.search_eligibility = "withheld".into();
    let spans = [blocked.clone(), clean.clone()];
    let evidence = SessionEvidence::new(&session.workspace_id, &session, &spans);
    assert!(legacy_support(&session, &spans, &row, None).is_none());
    assert!(evidence.support(&row).is_none());
    assert_eq!(evidence.context_builds.get(), 0);
    let reversed = [clean, blocked];
    let evidence = SessionEvidence::new(&session.workspace_id, &session, &reversed);
    let actual = evidence.support(&row).unwrap();
    assert_eq!(
        Some(actual.clone()),
        legacy_support(&session, &reversed, &row, None)
    );
    assert_eq!(actual.sources.len(), 1, "repeated endpoint is one source");
    assert_eq!(actual.context_sources.len(), 2, "context never drops a row");
    row.source_ids.push("missing".into());
    assert!(
        evidence.support(&row).is_none(),
        "cached context cannot fill a missing source"
    );
    assert_eq!(evidence.context_builds.get(), 1);
}

#[test]
fn structured_unicode_and_repeated_events_keep_exact_old_context_bytes() {
    let session = session();
    for structured in [false, true] {
        let text =
            "Always run cargo test --locked after changing src/support.rs. Check café output.";
        let body = if structured {
            serde_json::json!({"type":"assistant","content":text}).to_string()
        } else {
            text.to_owned()
        };
        let spans = [span("source", 1, &body), span("copy", 2, &body)];
        let mut row = template();
        row.source_ids = vec!["source".into()];
        let evidence = SessionEvidence::new(&session.workspace_id, &session, &spans);
        let expected = legacy_support(&session, &spans, &row, None).unwrap();
        for _ in 0..16 {
            assert_eq!(evidence.support(&row), Some(expected.clone()));
        }
        let single = legacy_support(&session, &spans[..1], &row, None).unwrap();
        assert_eq!(expected.payload_hash, single.payload_hash);
        assert_ne!(expected.context_sources, single.context_sources);
        assert_eq!(evidence.context_builds.get(), 1);
    }
}
