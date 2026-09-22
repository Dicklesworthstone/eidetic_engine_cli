//! Source-backed failed-to-fixed learning. Proposals never apply themselves.

use super::*;

#[path = "curate_session_arc_clauses.rs"]
mod clauses;
#[path = "curate_session_arc_sequence.rs"]
mod sequence;
#[path = "curate_session_arc_text.rs"]
mod text;

pub(super) fn sequence_candidates(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<ReviewSessionCandidate> {
    sequence::candidates(workspace_id, session, spans)
}

/// A failure and its repair may live in one imported CASS window. Keep the
/// original evidence ID, hash, and complete locator: sentence boundaries in an
/// excerpt are not transcript line boundaries and must not invent provenance.
pub(super) fn inline_candidates(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<ReviewSessionCandidate> {
    let mut candidates = Vec::new();
    let mut seen_ids = BTreeSet::new();
    for span in spans {
        if span.workspace_id != workspace_id || span.session_id != session.id {
            continue;
        }
        let Some(message) = text::message_text(&span.excerpt) else {
            continue;
        };
        for (failure, repair) in inline_pairs(message.as_ref()) {
            let mut failure_span = span.clone();
            failure_span.excerpt = failure.to_owned();
            let mut repair_span = span.clone();
            repair_span.excerpt = repair.to_owned();
            let topic = review_topic_key(&format!("{failure} {repair}"));
            let pair = build_session_arc_candidate_pair(
                workspace_id,
                session,
                &topic,
                &failure_span,
                &repair_span,
            );
            // Repeated observations (or equal compacted lesson content) must
            // not persist duplicate IDs or leave only one reciprocal member.
            // Keep the builder's content-bound identities and source package.
            if pair
                .iter()
                .any(|candidate| seen_ids.contains(&candidate.candidate_id))
            {
                continue;
            }
            for candidate in pair {
                seen_ids.insert(candidate.candidate_id.clone());
                candidates.push(candidate);
            }
        }
    }
    candidates
}

/// Walk the complete admitted message without combining independent episodes.
/// A successful repair consumes its failure; further successes cannot reuse
/// it. A later failure replaces an unresolved one, preserving the existing
/// nearest-failure rule rather than inventing links between interleaved tasks.
/// Each yielded half borrows one exact technical clause from the source text.
fn inline_pairs(excerpt: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut failure: Option<&str> = None;
    // Technical tokens and quoted commands stay intact. A bare occurrence of
    // both keywords in a single clause is not evidence of temporal ordering.
    clauses::split(excerpt).filter_map(move |part| {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        if let Some(previous) = failure
            && resolution_signal(part)
        {
            let explicit_pair = previous.to_ascii_lowercase().contains("failure arc:")
                && part.to_ascii_lowercase().contains("fix:");
            let previous_topic = review_topic_key(previous);
            if explicit_pair
                || (previous_topic != "noise" && previous_topic == review_topic_key(part))
            {
                failure = None;
                return Some((previous, part));
            }
        }
        if session_arc_failure_signal(part) {
            failure = Some(part);
        }
        None
    })
}

// Existing clause-boundary tests also pin the historical first-pair behavior.
#[cfg(test)]
fn inline_pair(excerpt: &str) -> Option<(&str, &str)> {
    inline_pairs(excerpt).next()
}

/// Negative or predicted repairs must not become positive lessons merely
/// because they mention `fixed`, `green`, or `passed`. This is conservative
/// lexical admission, not proof that an arbitrary natural-language claim is true.
pub(super) fn resolution_signal(excerpt: &str) -> bool {
    if !session_arc_resolution_signal(excerpt) {
        return false;
    }
    let lowercase = excerpt.to_ascii_lowercase().replace('’', "'");
    let words: Vec<_> = lowercase
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '\'')
        .map(|word| word.trim_matches('\''))
        .filter(|word| !word.is_empty())
        .collect();
    if words.iter().any(|word| {
        matches!(
            *word,
            "failed"
                | "failing"
                | "broken"
                | "blocked"
                | "timeout"
                | "panic"
                | "denied"
                | "unsuccessful"
                | "unverified"
                | "unresolved"
        )
    }) {
        return false;
    }
    !words.iter().enumerate().any(|(index, word)| {
        let negated = matches!(*word, "not" | "never" | "cannot" | "no") || word.ends_with("n't");
        let predicted = matches!(
            *word,
            "will" | "would" | "should" | "could" | "may" | "might"
        );
        (negated || predicted)
            && words.iter().skip(index + 1).take(6).any(|next| {
                matches!(
                    *next,
                    "fixed"
                        | "fix"
                        | "green"
                        | "passed"
                        | "passing"
                        | "repair"
                        | "repaired"
                        | "resolved"
                        | "verified"
                        | "works"
                        | "succeed"
                        | "succeeded"
                        | "successful"
                )
            })
    })
}

/// Respect the public limit without persisting only half of a linked proposal.
/// Ranked ordinary candidates may fill a slot too small for an entire pair.
pub(super) fn limit_complete_pairs(candidates: &mut Vec<ReviewSessionCandidate>, limit: usize) {
    let by_id: BTreeMap<_, _> = candidates
        .iter()
        .map(|candidate| (candidate.candidate_id.as_str(), candidate))
        .collect();
    let mut retained = BTreeSet::new();
    for candidate in candidates.iter() {
        if retained.contains(&candidate.candidate_id) || retained.len() >= limit {
            continue;
        }
        if let Some(arc) = &candidate.session_arc {
            let Some(peer) = by_id.get(arc.linked_candidate_id.as_str()) else {
                continue;
            };
            let reciprocal = peer.session_arc.as_ref().is_some_and(|peer_arc| {
                peer_arc.arc_id == arc.arc_id
                    && peer_arc.linked_candidate_id == candidate.candidate_id
            });
            if !reciprocal || limit.saturating_sub(retained.len()) < 2 {
                continue;
            }
            retained.insert(peer.candidate_id.clone());
        }
        retained.insert(candidate.candidate_id.clone());
    }
    candidates.retain(|candidate| retained.contains(&candidate.candidate_id));
}

/// Verified sharing of source evidence, independently of reciprocal linkage.
/// `memory` is the immutable first owner used by validation and previews. For
/// multiple episodes in one window it need not be this candidate's counterpart.
/// Only `linked_memory` can supply the other endpoint of a failure/repair link.
#[derive(Clone, Debug)]
pub(super) struct AppliedPeer {
    pub memory: StoredMemory,
    pub shared_evidence_ids: BTreeSet<String>,
    linked_memory: Option<StoredMemory>,
    arc: ReviewSessionArcMetadata,
}

fn pair_issue(message: impl Into<String>) -> CurateValidationIssue {
    validation_issue(
        "session_arc_pair_invalid",
        message,
        "Inspect both session-arc candidates and their source evidence; re-propose a current pair before applying.",
    )
}

pub(super) fn applied_peer(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
) -> Result<Option<AppliedPeer>, CurateValidationIssue> {
    let Some(raw) = stored.derivation_metadata_json.as_deref() else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(None);
    };
    if value
        .pointer("/producer/producerPayload/sessionArc")
        .is_none()
    {
        return Ok(None);
    }
    let metadata = parse_derivation_metadata(stored)?;
    if metadata.producer.producer != "review_session" {
        return Err(pair_issue(
            "Session-arc metadata requires a review_session producer.",
        ));
    }
    let refs = parse_derivation_source_refs(stored)?;
    if refs.is_empty()
        || refs.len() > 2
        || refs
            .iter()
            .any(|source| source.kind != DerivationSourceKind::EvidenceSpan)
    {
        return Err(pair_issue(
            "A session arc must have one or two evidence-span sources.",
        ));
    }
    let mut spans = Vec::new();
    for source in &refs {
        let span = connection
            .get_evidence_span(&source.id)
            .map_err(|error| pair_issue(format!("Cannot read session-arc source: {error}")))?
            .ok_or_else(|| pair_issue("A session-arc source is missing."))?;
        if span.workspace_id != stored.workspace_id || span.content_hash != source.content_hash {
            return Err(pair_issue("Session-arc source workspace or hash changed."));
        }
        spans.push(span);
    }
    let session = connection
        .get_session(&spans[0].session_id)
        .map_err(|error| pair_issue(format!("Cannot read session-arc provenance: {error}")))?
        .ok_or_else(|| pair_issue("Session-arc provenance is missing."))?;
    if session.workspace_id != stored.workspace_id
        || spans.iter().any(|span| {
            span.session_id != session.id
                || !span.is_search_admitted_for_session(&stored.workspace_id, &session)
        })
    {
        return Err(pair_issue(
            "Session-arc sources no longer share admitted session provenance.",
        ));
    }
    let expected = build_session_arc_candidates(&stored.workspace_id, &session, &spans, 0.0);
    let current = expected
        .iter()
        .find(|candidate| candidate.candidate_id == stored.id)
        .ok_or_else(|| {
            pair_issue("Session-arc candidate cannot be reconstructed from its current evidence.")
        })?;
    verify_candidate(connection, stored, current, &session)?;
    let arc = current
        .session_arc
        .as_ref()
        .ok_or_else(|| pair_issue("Missing reconstructed session arc."))?;
    let expected_peer = expected
        .iter()
        .find(|candidate| candidate.candidate_id == arc.linked_candidate_id)
        .ok_or_else(|| pair_issue("Missing reciprocal session-arc proposal."))?;
    let linked_memory = match connection
        .get_curation_candidate(&stored.workspace_id, &arc.linked_candidate_id)
        .map_err(|error| pair_issue(format!("Cannot inspect paired candidate: {error}")))?
    {
        Some(peer) => applied_memory(connection, &peer, expected_peer, &session)?,
        None => None,
    };

    // Additional episodes may reuse exactly one complete window, never an
    // arbitrary collection of already-owned spans. Reconstruct both lessons
    // from that window and prove its owner was explicitly applied. Its peer
    // may still be pending/rejected: source sharing does not accept or link it.
    let memory = if let [span] = spans.as_slice()
        && is_inline_candidate(current, span)
        && let Some(owner_id) = span.memory_id.as_deref()
    {
        match linked_memory.as_ref() {
            Some(memory) if memory.id == owner_id => memory.clone(),
            _ => applied_window_owner(connection, &session, span, &expected, owner_id)?,
        }
    } else {
        // Two-window arcs retain the exact reciprocal-pair exception. An
        // unowned first lesson needs neither a peer nor a sharing exception.
        let Some(memory) = linked_memory.as_ref() else {
            return Ok(None);
        };
        memory.clone()
    };
    let shared_evidence_ids = spans
        .iter()
        .filter(|span| span.memory_id.as_deref() == Some(memory.id.as_str()))
        .map(|span| span.id.clone())
        .collect();
    Ok(Some(AppliedPeer {
        memory,
        shared_evidence_ids,
        linked_memory,
        arc: arc.clone(),
    }))
}

fn is_inline_candidate(candidate: &ReviewSessionCandidate, span: &StoredEvidenceSpan) -> bool {
    candidate.source_ids.len() == 1
        && candidate.source_ids[0] == span.id
        && candidate.session_arc.as_ref().is_some_and(|arc| {
            arc.failure_span.evidence_span_id == span.id
                && arc.resolution_span.evidence_span_id == span.id
        })
}

/// This is read-only proof, not an approval operation. Both proposal identity
/// and the applied memory must still match the current source reconstruction.
fn applied_memory(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
    expected: &ReviewSessionCandidate,
    session: &StoredSession,
) -> Result<Option<StoredMemory>, CurateValidationIssue> {
    verify_candidate(connection, stored, expected, session)?;
    if stored.status != CandidateStatus::Applied.as_str() {
        return Ok(None);
    }
    let memory = load_create_derived_replay_memory(connection, stored)?;
    let expected_content =
        crate::policy::redact_secret_like_content(&expected.proposed_content).content;
    if memory.tombstoned_at.is_some()
        || memory.level != "procedural"
        || memory.kind != review_candidate_derived_memory_kind(expected)
        || memory.content != expected_content
    {
        return Err(pair_issue(
            "An applied session-arc memory was retired or changed; it cannot authorize source sharing or linkage.",
        ));
    }
    Ok(Some(memory))
}

/// Follow the immutable source owner to its one creation audit. Do not scan
/// every proposed episode and replay each: admission is bounded to this window
/// plus the owner/counterpart, irrespective of the number of proposed lessons.
fn applied_window_owner(
    connection: &DbConnection,
    session: &StoredSession,
    span: &StoredEvidenceSpan,
    expected: &[ReviewSessionCandidate],
    owner_id: &str,
) -> Result<StoredMemory, CurateValidationIssue> {
    let audits = connection
        .list_audit_by_target("memory", owner_id, None)
        .map_err(|error| pair_issue(format!("Cannot inspect source-owner creation: {error}")))?;
    let mut creations = audits
        .iter()
        .filter(|audit| audit.action == audit_actions::MEMORY_CREATE);
    let audit = creations
        .next()
        .ok_or_else(|| pair_issue("The source owner has no memory-creation audit."))?;
    if creations.next().is_some()
        || audit.workspace_id.as_deref() != Some(session.workspace_id.as_str())
    {
        return Err(pair_issue(
            "The source owner's creation is ambiguous or belongs to another workspace.",
        ));
    }
    let details: serde_json::Value = serde_json::from_str(
        audit
            .details
            .as_deref()
            .ok_or_else(|| pair_issue("The source-owner creation audit has no details."))?,
    )
    .map_err(|error| pair_issue(format!("Invalid source-owner creation audit: {error}")))?;
    if details["schema"] != "ee.audit.derived_memory_created.v1"
        || details["createdMemoryId"].as_str() != Some(owner_id)
        || details["producer"] != "review_session"
    {
        return Err(pair_issue(
            "The source owner was not created by an explicitly applied session-arc candidate.",
        ));
    }
    let expected_owner = expected
        .iter()
        .find(|candidate| {
            details["candidateId"].as_str() == Some(candidate.candidate_id.as_str())
                && is_inline_candidate(candidate, span)
        })
        .ok_or_else(|| {
            pair_issue("The source owner is not a reconstructed episode of this exact window.")
        })?;
    let owner = connection
        .get_curation_candidate(&session.workspace_id, &expected_owner.candidate_id)
        .map_err(|error| pair_issue(format!("Cannot inspect source-owner candidate: {error}")))?
        .ok_or_else(|| pair_issue("The source-owner candidate is missing."))?;
    let memory = applied_memory(connection, &owner, expected_owner, session)?
        .ok_or_else(|| pair_issue("The source-owner candidate has not been explicitly applied."))?;
    let metadata = parse_derivation_metadata(&owner)?;
    let source_refs: serde_json::Value = serde_json::from_str(
        owner
            .derivation_source_refs_json
            .as_deref()
            .ok_or_else(|| pair_issue("The source-owner candidate has no source package."))?,
    )
    .map_err(|error| pair_issue(format!("Invalid source-owner source package: {error}")))?;
    if memory.id != owner_id
        || details.get("sourceRefs") != Some(&source_refs)
        || details.get("producerPayload") != metadata.producer.producer_payload.as_ref()
    {
        return Err(pair_issue(
            "The source-owner creation audit does not match its reconstructed memory and evidence package.",
        ));
    }
    Ok(memory)
}

fn verify_candidate(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
    expected: &ReviewSessionCandidate,
    session: &StoredSession,
) -> Result<(), CurateValidationIssue> {
    let (refs, metadata) = review_bootstrap_derivation_package(
        connection,
        &stored.workspace_id,
        expected,
        Some(session),
    )
    .map_err(|error| pair_issue(error.message()))?;
    if stored.workspace_id != session.workspace_id
        || stored.candidate_type != CandidateType::CreateDerivedMemory.as_str()
        || stored.target_memory_id.is_some()
        || stored.source_type != persisted_review_candidate_source_type(expected)
        || stored.source_id.as_deref() != Some(expected.source_ids.join(",").as_str())
        || stored.proposed_content.as_deref() != Some(expected.proposed_content.as_str())
        || stored.derivation_source_refs_json.as_deref() != Some(refs.as_str())
        || stored.derivation_metadata_json.as_deref() != Some(metadata.as_str())
    {
        return Err(pair_issue(
            "Session-arc content, role, reciprocal identity, or source package was modified.",
        ));
    }
    Ok(())
}

pub(super) fn pair_domain_error(issue: CurateValidationIssue) -> DomainError {
    DomainError::Storage {
        message: format!("{}: {}", issue.code, issue.message),
        repair: Some(issue.repair),
    }
}

pub(super) fn planned_pair_link(
    created_memory_id: &str,
    peer: Option<&AppliedPeer>,
) -> Option<CurateShowPlannedSessionArcLink> {
    let peer = peer?;
    let linked_memory = peer.linked_memory.as_ref()?;
    let (rule_id, anti_id) = if peer.arc.role == "rule" {
        (created_memory_id, linked_memory.id.as_str())
    } else {
        (linked_memory.id.as_str(), created_memory_id)
    };
    Some(CurateShowPlannedSessionArcLink {
        link_id: generate_suggested_link_id(rule_id, anti_id, "related"),
        src_memory_id: rule_id.to_owned(),
        dst_memory_id: anti_id.to_owned(),
        relation: "related".to_owned(),
        directed: false,
        arc_id: peer.arc.arc_id.clone(),
    })
}

/// Called inside the existing curation transaction, after both memories exist.
/// Do not replace the evidence's first accepted owner or accept the peer here.
/// Both memories retain exact evidence hashes in their own creation audits;
/// this typed, audited edge makes the learned failure/repair pair traversable.
pub(super) fn persist_pair_link(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
    created: &ApplyDerivedMemoryInput,
    peer: Option<&AppliedPeer>,
    applied_at: &str,
    actor: &str,
) -> Result<(), DomainError> {
    let Some(peer) = peer else {
        return Ok(());
    };
    let Some(linked_memory) = peer.linked_memory.as_ref() else {
        return Ok(());
    };
    let Some(link) = planned_pair_link(&created.memory_id, Some(peer)) else {
        return Ok(());
    };
    let rule_id = &link.src_memory_id;
    let anti_id = &link.dst_memory_id;
    let link_id = link.link_id;
    let details = serde_json::json!({
        "schema": "ee.memory_link.session_arc.v1",
        "arcId": peer.arc.arc_id,
        "linkage": "failed_to_fixed",
        "ruleMemoryId": rule_id,
        "antiPatternMemoryId": anti_id,
        "ruleCandidateId": peer.arc.proposed_rule_candidate_id,
        "antiPatternCandidateId": peer.arc.proposed_anti_pattern_candidate_id,
        "sessionProvenance": peer.arc.session_provenance,
        "failureSpan": peer.arc.failure_span,
        "resolutionSpan": peer.arc.resolution_span,
        "linkId": link_id,
    })
    .to_string();
    connection
        .insert_memory_link(
            &link_id,
            &CreateMemoryLinkInput {
                src_memory_id: rule_id.clone(),
                dst_memory_id: anti_id.clone(),
                relation: MemoryLinkRelation::Related,
                weight: 1.0,
                confidence: created.memory.confidence.min(linked_memory.confidence),
                directed: false,
                evidence_count: u32::try_from(created.evidence_refs.len()).unwrap_or(u32::MAX),
                last_reinforced_at: Some(applied_at.to_owned()),
                source: MemoryLinkSource::Agent,
                created_by: Some(actor.to_owned()),
                metadata_json: Some(details.clone()),
            },
        )
        .map_err(map_create_derived_insert_memory_link_db_error)?;
    connection
        .insert_audit(
            &generate_audit_id(),
            &CreateAuditInput {
                workspace_id: Some(stored.workspace_id.clone()),
                actor: Some(actor.to_owned()),
                action: audit_actions::MEMORY_LINK_CREATE.to_owned(),
                target_type: Some("memory_link".to_owned()),
                target_id: Some(link_id),
                details: Some(details),
            },
        )
        .map_err(map_create_derived_insert_audit_db_error)?;
    Ok(())
}

#[cfg(test)]
mod episode_tests {
    use super::*;

    const FIRST_FAILURE: &str = "Failure arc: M7.cache.lookup in src/cache.rs failed.";
    const FIRST_REPAIR: &str =
        "Fix: M7.cache.lookup was repaired by selecting stable identity bytes.";
    const SECOND_FAILURE: &str = "Failure arc: M8.index.publish in src/index.rs failed.";
    const SECOND_REPAIR: &str =
        "Fix: M8.index.publish was repaired by publishing the complete generation.";

    #[test]
    fn every_complete_episode_survives_in_source_order() {
        let source = format!("{FIRST_FAILURE}\n{FIRST_REPAIR}\n{SECOND_FAILURE}\n{SECOND_REPAIR}");
        assert_eq!(
            inline_pairs(&source).collect::<Vec<_>>(),
            [
                (FIRST_FAILURE, FIRST_REPAIR),
                (SECOND_FAILURE, SECOND_REPAIR)
            ]
        );
        assert_eq!(inline_pair(&source), Some((FIRST_FAILURE, FIRST_REPAIR)));
    }

    #[test]
    fn resolved_failures_cannot_be_reused_by_later_successes() {
        let source = format!("{FIRST_FAILURE} {FIRST_REPAIR} {FIRST_REPAIR} {SECOND_REPAIR}");
        assert_eq!(
            inline_pairs(&source).collect::<Vec<_>>(),
            [(FIRST_FAILURE, FIRST_REPAIR)]
        );
    }

    #[test]
    fn latest_unresolved_failure_is_the_only_candidate_for_a_repair() {
        let source = format!("{FIRST_FAILURE} {SECOND_FAILURE} {SECOND_REPAIR}");
        assert_eq!(
            inline_pairs(&source).collect::<Vec<_>>(),
            [(SECOND_FAILURE, SECOND_REPAIR)]
        );
    }

    #[test]
    fn leading_successes_and_unresolved_tails_do_not_manufacture_episodes() {
        let source = format!("{SECOND_REPAIR} {FIRST_FAILURE} {FIRST_REPAIR} {SECOND_FAILURE}");
        assert_eq!(
            inline_pairs(&source).collect::<Vec<_>>(),
            [(FIRST_FAILURE, FIRST_REPAIR)]
        );
        assert!(inline_pairs(SECOND_REPAIR).next().is_none());
        assert!(inline_pairs(SECOND_FAILURE).next().is_none());
        assert!(inline_pairs("").next().is_none());
    }

    #[test]
    fn negative_and_predicted_repairs_do_not_close_an_episode() {
        for unobserved in [
            "Fix: the cache isn't fixed.",
            "Fix: the cache was not repaired.",
            "Fix: the cache will be fixed by stable identity bytes.",
            "Fix: the cache might be repaired by stable identity bytes.",
        ] {
            let source = format!("{FIRST_FAILURE} {unobserved}");
            assert!(inline_pairs(&source).next().is_none(), "{unobserved}");
            let source = format!("{source} {SECOND_FAILURE} {SECOND_REPAIR}");
            assert_eq!(
                inline_pairs(&source).collect::<Vec<_>>(),
                [(SECOND_FAILURE, SECOND_REPAIR)]
            );
        }
    }

    #[test]
    fn earlier_repairs_remain_local_when_later_failures_are_added() {
        let first = format!("{FIRST_FAILURE} {FIRST_REPAIR}");
        let combined = format!("{first} {SECOND_FAILURE} {SECOND_REPAIR}");
        let expected = inline_pairs(&first).collect::<Vec<_>>();
        assert_eq!(
            inline_pairs(&combined).take(1).collect::<Vec<_>>(),
            expected
        );
        for (failure, repair) in inline_pairs(&combined) {
            assert!(combined.contains(failure));
            assert!(combined.contains(repair));
            assert!(!repair.contains("Failure arc:"));
        }
    }

    #[test]
    fn repeated_source_episodes_are_extracted_without_cross_pairing() {
        let source = format!("{FIRST_FAILURE} {FIRST_REPAIR} ").repeat(64);
        let pairs: Vec<_> = inline_pairs(&source).collect();
        assert_eq!(pairs.len(), 64);
        assert!(
            pairs
                .iter()
                .all(|pair| *pair == (FIRST_FAILURE, FIRST_REPAIR))
        );
    }

    #[test]
    fn technical_clauses_and_unicode_survive_multiple_episodes_exactly() {
        let failure = "Failure arc: 資料 `cache.read(\"a.b\"); cache.close()` failed.";
        let repair = "Fix: ``cache.write(`key`, 2.4); cache.close()`` repaired 資料 lookup.";
        let source = format!("{failure}\r\n{repair}\r\n{SECOND_FAILURE}\r\n{SECOND_REPAIR}");
        assert_eq!(
            inline_pairs(&source).collect::<Vec<_>>(),
            [(failure, repair), (SECOND_FAILURE, SECOND_REPAIR)]
        );
    }
}
