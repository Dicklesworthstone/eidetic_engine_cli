//! Source-backed failed-to-fixed learning. Proposals never apply themselves.

use super::*;

/// A failure and its repair may live in one imported CASS window. Keep the
/// original evidence ID, hash, and complete locator: sentence boundaries in an
/// excerpt are not transcript line boundaries and must not invent provenance.
pub(super) fn inline_candidates(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<ReviewSessionCandidate> {
    let mut candidates = Vec::new();
    for span in spans {
        if span.workspace_id != workspace_id || span.session_id != session.id {
            continue;
        }
        let Some((failure, repair)) = inline_pair(&span.excerpt) else {
            continue;
        };
        let mut failure_span = span.clone();
        failure_span.excerpt = failure.to_owned();
        let mut repair_span = span.clone();
        repair_span.excerpt = repair.to_owned();
        let topic = review_topic_key(&format!("{failure} {repair}"));
        candidates.extend(build_session_arc_candidate_pair(
            workspace_id,
            session,
            &topic,
            &failure_span,
            &repair_span,
        ));
    }
    candidates
}

fn inline_pair(excerpt: &str) -> Option<(&str, &str)> {
    let mut failure: Option<&str> = None;
    // Split only at visible clause/sentence boundaries. A bare occurrence of
    // both keywords in a single clause is not evidence of temporal ordering.
    for part in excerpt.split_inclusive(['\n', ';', '.']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
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
                return Some((previous, part));
            }
        }
        if session_arc_failure_signal(part) {
            failure = Some(part);
        }
    }
    None
}

/// Negative or failed repair attempts must not become positive lessons merely
/// because they mention `fixed`, `green`, or `passed`.
pub(super) fn resolution_signal(excerpt: &str) -> bool {
    if !session_arc_resolution_signal(excerpt) {
        return false;
    }
    let lowercase = excerpt.to_ascii_lowercase();
    let words: Vec<_> = lowercase
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if words.iter().any(|word| {
        matches!(
            *word,
            "failed" | "failing" | "broken" | "blocked" | "timeout" | "panic" | "denied"
        )
    }) {
        return false;
    }
    !words.iter().enumerate().any(|(index, word)| {
        matches!(*word, "not" | "never" | "cannot")
            && words.iter().skip(index + 1).take(3).any(|next| {
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

/// The only evidence-sharing exception: the other half of this exact,
/// source-reconstructed pair has already been explicitly applied and its
/// original live memory is identified by an unambiguous creation audit.
#[derive(Clone, Debug)]
pub(super) struct AppliedPeer {
    pub memory: StoredMemory,
    pub shared_evidence_ids: BTreeSet<String>,
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
    let Some(peer) = connection
        .get_curation_candidate(&stored.workspace_id, &arc.linked_candidate_id)
        .map_err(|error| pair_issue(format!("Cannot inspect paired candidate: {error}")))?
    else {
        // The other side is still a proposal, not implicit authorization to
        // create a second memory. Applying this side alone remains permitted.
        return Ok(None);
    };
    verify_candidate(connection, &peer, expected_peer, &session)?;
    if peer.status != CandidateStatus::Applied.as_str() {
        return Ok(None);
    }
    let memory = load_create_derived_replay_memory(connection, &peer)?;
    let expected_content =
        crate::policy::redact_secret_like_content(&expected_peer.proposed_content).content;
    if memory.tombstoned_at.is_some()
        || memory.level != "procedural"
        || memory.kind != review_candidate_derived_memory_kind(expected_peer)
        || memory.content != expected_content
    {
        return Err(pair_issue(
            "The applied peer memory was retired or changed; evidence cannot be reassigned to it.",
        ));
    }
    let shared_evidence_ids = spans
        .iter()
        .filter(|span| span.memory_id.as_deref() == Some(memory.id.as_str()))
        .map(|span| span.id.clone())
        .collect();
    Ok(Some(AppliedPeer {
        memory,
        shared_evidence_ids,
        arc: arc.clone(),
    }))
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
    if stored.candidate_type != CandidateType::CreateDerivedMemory.as_str()
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
    let (rule_id, anti_id) = if peer.arc.role == "rule" {
        (created_memory_id, peer.memory.id.as_str())
    } else {
        (peer.memory.id.as_str(), created_memory_id)
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
                confidence: created.memory.confidence.min(peer.memory.confidence),
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
