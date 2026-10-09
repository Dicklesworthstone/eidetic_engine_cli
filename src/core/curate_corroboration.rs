//! Bounded exact corroboration of session-derived proposals.
//!
//! Scores are explicit heuristic support tiers, not calibrated probabilities.
//! A session, its repeated windows, and an imported alias are one observation.
//! The anchor still owns the proposed lesson; corroboration never transfers
//! another session's evidence to the resulting memory.

use super::*;
use crate::db::{CassSourceCommitment, ReviewCorroborationLimits};
use serde::{Deserialize, Serialize};

const SCHEMA: &str = "ee.review.corroboration.v1";
const SCORE_KIND: &str = "bounded_exact_session_support_heuristic";
const MAX_SUPPORT: usize = 3;
// The historical CASS importer could retain an unmarked UTF-8 prefix up to
// three bytes below its 64 KiB cap. Such a prefix cannot prove a complete lesson.
const LEGACY_EXCERPT_CAP: usize = 65_536;
const LIMITS: ReviewCorroborationLimits = ReviewCorroborationLimits {
    sessions: 32,
    spans_per_session: 256,
    total_bytes: 8 * 1024 * 1024,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReviewCorroboration {
    schema: String,
    score_kind: String,
    signature: String,
    sessions: Vec<SupportingSession>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SupportingSession {
    session_id: String,
    upstream_hash: String,
    source_path_hash: Option<String>,
    payload_hash: String,
    candidate_id: String,
    sources: Vec<SupportingSource>,
    context_sources: Vec<SupportingSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SupportingSource {
    evidence_span_id: String,
    content_hash: String,
    start_line: u32,
    end_line: u32,
    identity_hash: String,
}

fn digest(parts: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.review.exact_support.v1\0");
    for part in parts {
        let part = part.as_ref();
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

impl ReviewCorroboration {
    pub(super) fn lesson() -> Self {
        // Presentation chooses only a few short sentences. The scorer binds
        // this seed to complete original projected records, never that display.
        Self::pending(digest(["lesson"]))
    }

    pub(super) fn arc(failure: &str, repair: &str) -> Self {
        Self::pending(digest(["ordered_failure_repair", failure, repair]))
    }

    fn pending(signature: String) -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            score_kind: SCORE_KIND.to_owned(),
            signature,
            sessions: Vec::new(),
        }
    }

    pub(super) fn recorded(&self) -> bool {
        !self.sessions.is_empty()
    }

    fn confidence(&self) -> f32 {
        match self.sessions.len() {
            0 | 1 => 0.6,
            2 => 0.7,
            _ => 0.8,
        }
    }
}

fn domain(error: impl std::fmt::Display) -> DomainError {
    DomainError::Storage {
        message: format!("Failed to check session corroboration: {error}"),
        repair: Some("Inspect the session evidence and retry ee review session.".to_owned()),
    }
}

fn issue(error: impl std::fmt::Display) -> CurateValidationIssue {
    validation_issue(
        "review_corroboration_changed",
        format!("Recorded session corroboration cannot be verified: {error}"),
        "Review the current source sessions; historical proposal provenance is not rewritten.",
    )
}

/// Keep every source record as a structural barrier, but never let an
/// instruction-risk or invalid source session supply a confidence increment.
fn eligible_session(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> bool {
    !spans.is_empty()
        && session.workspace_id == workspace_id
        && spans.iter().all(|span| {
            span.producer_kind == EvidenceProducerKind::CassImport.as_str()
                && complete_source(span)
                && (span.is_derivation_admitted_for_session(workspace_id, session)
                    || span.is_class_a_derivation_readable(workspace_id, session))
                && review_learning_text(span).is_none_or(|text| !explicitly_refuted(&text))
        })
}

fn explicitly_refuted(text: &str) -> bool {
    let lower = text.to_ascii_lowercase().replace('’', "'");
    [
        "this rule is wrong",
        "this advice is wrong",
        "incorrect advice",
        "do not follow",
        "don't follow",
        "retract this",
        "retracted",
        "refuted",
        "failed again",
        "did not fix",
        "didn't fix",
        "does not fix",
        "doesn't fix",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn complete_source(span: &StoredEvidenceSpan) -> bool {
    span.excerpt.len() < LEGACY_EXCERPT_CAP - 3
        && !span.excerpt.contains("[TRUNCATED]")
        && !span.excerpt.contains("[REDACTED:truncated_source]")
        && !span.excerpt.contains("external_ingestion_withheld")
        && CassSourceCommitment::from_metadata(span.metadata_json.as_deref()).is_ok_and(|source| {
            source.is_none_or(|source| source.byte_length < (LEGACY_EXCERPT_CAP - 3) as u64)
        })
}

/// The complete contributing records keep qualifications, case and numbers
/// that the short proposal omits. Repeated identical records are one statement;
/// ordered failure/repair clauses remain part of the seed for an arc.
fn full_signature(
    candidate: &ReviewSessionCandidate,
    spans: &[StoredEvidenceSpan],
) -> Option<String> {
    let seed = &candidate.corroboration.as_ref()?.signature;
    let mut sources = candidate
        .source_ids
        .iter()
        .map(|id| spans.iter().find(|span| span.id == *id))
        .collect::<Option<Vec<_>>>()?;
    sources.sort_by(|left, right| session_arc_span_order(left, right));
    let mut parts = vec![seed.clone()];
    let mut seen = BTreeSet::new();
    for source in sources {
        if !complete_source(source) {
            return None;
        }
        let text = review_learning_text(source)?;
        let record = digest([source.role.as_deref().unwrap_or(""), text.as_ref()]);
        if seen.insert(record.clone()) {
            parts.push(record);
        }
    }
    (parts.len() > 1).then(|| digest(parts))
}

/// Source facts do not stop being observations when an accepted proposal
/// attaches them to a memory. Ignore ownership only for this in-memory
/// reconstruction; neither the row nor the resulting memory ownership changes.
fn source_candidates(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<ReviewSessionCandidate> {
    let mut grouped: BTreeMap<String, Vec<&StoredEvidenceSpan>> = BTreeMap::new();
    for span in spans {
        let Some(text) = review_learning_text(span) else {
            continue;
        };
        let topic = review_topic_key(&text);
        if topic != "noise" {
            grouped.entry(topic).or_default().push(span);
        }
    }
    let mut candidates = grouped
        .into_iter()
        .filter_map(|(topic, mut sources)| {
            sources.sort_by(|left, right| session_arc_span_order(left, right));
            build_bootstrap_candidate(workspace_id, session, &topic, &sources)
        })
        .collect::<Vec<_>>();
    candidates.extend(build_session_arc_candidates(
        workspace_id,
        session,
        spans,
        0.0,
    ));
    candidates
}

fn reconstruct_support(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
    expected: &SupportingSession,
) -> Option<ReviewSessionCandidate> {
    // Reconstruct the recorded source group, not a current ownership cluster.
    // Additional unrelated turns cannot replace a named supporting source.
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
    // The complete ordered transcript retains every class-A barrier, so
    // selecting just two named endpoints cannot invent a failure/repair link.
    build_session_arc_candidates(workspace_id, session, spans, 0.0)
        .into_iter()
        .find(|candidate| candidate.candidate_id == expected.candidate_id)
}

fn support(
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
    candidate: &ReviewSessionCandidate,
) -> Option<SupportingSession> {
    support_in_context(session, spans, candidate, None)
}

fn source_reference(span: &StoredEvidenceSpan) -> SupportingSource {
    SupportingSource {
        evidence_span_id: span.id.clone(),
        content_hash: span.content_hash.clone(),
        start_line: span.start_line,
        end_line: span.end_line,
        identity_hash: digest([
            span.cass_span_id.as_str(),
            span.start_byte
                .map(|value| value.to_string())
                .as_deref()
                .unwrap_or(""),
            span.end_byte
                .map(|value| value.to_string())
                .as_deref()
                .unwrap_or(""),
            span.upstream_ref_hash.as_deref().unwrap_or(""),
            span.metadata_json.as_deref().unwrap_or(""),
        ]),
    }
}

fn support_in_context(
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
    // An alias carrying a byte-identical conversation is not another
    // independent observation. Ignore local IDs and JSON envelopes for text
    // records; preserve complete screened tool payloads as barriers.
    let mut payload = vec!["complete_session".to_owned()];
    let mut seen_events = BTreeSet::new();
    let mut context_sources = Vec::new();
    // Capture an observed prefix, not a promise that a session will never
    // grow. New rows within that prefix fail equality; ordinary later turns
    // do not rewrite the support snapshot. The caller still screens all
    // current rows for refutation and admission.
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
        // A copied transcript with repeated windows is still a copied
        // transcript. Removing duplicate events can only reduce support.
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

fn independent(support: &SupportingSession, previous: &[SupportingSession]) -> bool {
    previous.iter().all(|prior| {
        support.session_id != prior.session_id
            && support.upstream_hash != prior.upstream_hash
            && support.payload_hash != prior.payload_hash
            && !(support.source_path_hash.is_some()
                && support.source_path_hash == prior.source_path_hash)
    })
}

fn rejected(
    connection: &DbConnection,
    workspace_id: &str,
    candidate: &ReviewSessionCandidate,
) -> Result<bool, DomainError> {
    // Reviewing the anti-pattern and its paired rule are separate choices.
    // Rejecting one role does not retract the other recorded observation.
    Ok(connection
        .get_curation_candidate(workspace_id, &candidate.candidate_id)
        .map_err(domain)?
        .is_some_and(|stored| stored.status == CandidateStatus::Rejected.as_str()))
}

/// Called before the caller's confidence floor or candidate limit. One bounded
/// read snapshot supplies the exact source proof. Historical rows are frozen
/// before selection, so reports cannot advertise fresh support as persisted.
pub(super) fn score_candidates(
    connection: &DbConnection,
    workspace_id: &str,
    session: &StoredSession,
    candidates: &mut Vec<ReviewSessionCandidate>,
    min_confidence: f32,
    limit: u32,
) -> Result<(), DomainError> {
    if candidates.is_empty() {
        return Ok(());
    }
    connection.begin_read_snapshot().map_err(domain)?;
    struct Snapshot<'a>(&'a DbConnection, bool);
    impl Drop for Snapshot<'_> {
        fn drop(&mut self) {
            if self.1 {
                let _ = self.0.rollback_read_snapshot();
            }
        }
    }
    let mut snapshot = Snapshot(connection, true);
    let anchor = if candidates
        .iter()
        .any(|candidate| candidate.corroboration.is_some())
    {
        connection
            .review_corroboration_sessions(
                workspace_id,
                &session.id,
                &[session.id.as_str()],
                LIMITS,
            )
            .map_err(domain)?
    } else {
        Vec::new()
    };
    if let Some((anchor_session, anchor_spans, anchor_bytes)) = anchor.first()
        && eligible_session(workspace_id, anchor_session, anchor_spans)
    {
        for candidate in candidates.iter_mut() {
            if candidate.corroboration.is_none() {
                continue;
            }
            let Some(anchor_support) = support(anchor_session, anchor_spans, candidate) else {
                continue;
            };
            let Some(current) =
                reconstruct_support(workspace_id, anchor_session, anchor_spans, &anchor_support)
            else {
                continue;
            };
            if same_proposal(candidate, &current)
                && !rejected(connection, workspace_id, &current)?
                && let Some(signature) = full_signature(&current, anchor_spans)
                && let Some(proof) = candidate.corroboration.as_mut()
            {
                proof.signature = signature;
                proof.sessions = vec![anchor_support];
            }
        }
        let other_limits = ReviewCorroborationLimits {
            total_bytes: LIMITS.total_bytes.saturating_sub(*anchor_bytes),
            ..LIMITS
        };
        let others = connection
            .review_corroboration_sessions(workspace_id, &session.id, &[], other_limits)
            .map_err(domain)?;
        for (other_session, spans, _) in &others {
            if !eligible_session(workspace_id, other_session, spans) {
                continue;
            }
            let other_candidates = source_candidates(workspace_id, other_session, spans);
            for candidate in candidates.iter_mut() {
                let Some(proof) = candidate.corroboration.as_mut() else {
                    continue;
                };
                if proof.sessions.is_empty() || proof.sessions.len() >= MAX_SUPPORT {
                    continue;
                }
                for other in &other_candidates {
                    if other.candidate_kind != candidate.candidate_kind
                        || full_signature(other, spans).as_ref() != Some(&proof.signature)
                        || rejected(connection, workspace_id, other)?
                    {
                        continue;
                    }
                    if let Some(additional) = support(other_session, spans, other)
                        && independent(&additional, &proof.sessions)
                    {
                        proof.sessions.push(additional);
                        break;
                    }
                }
            }
        }
    }
    for candidate in candidates.iter_mut() {
        if let Some(proof) = &candidate.corroboration {
            candidate.confidence = proof.confidence();
            candidate.proposed_confidence = proof.confidence();
            let sources = proof
                .sessions
                .iter()
                .map(|support| {
                    format!(
                        "cass-session://{} [{}]",
                        support.session_id,
                        support
                            .sources
                            .iter()
                            .map(|source| format!(
                                "{}#L{}-{}",
                                source.evidence_span_id, source.start_line, source.end_line
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            if proof.sessions.is_empty() {
                candidate.reason.push_str(" Single-session heuristic only: complete corroboration could not be established under the admission and read bounds.");
            } else {
                candidate.reason.push_str(&format!(
                    " Bounded exact corroboration: {} independently identified session(s), heuristic confidence {:.2}; not a calibrated probability or exhaustive consensus. {}",
                    proof.sessions.len(), proof.confidence(), sources,
                ));
            }
        }
        freeze_existing(connection, workspace_id, candidate)?;
    }
    connection.commit_read_snapshot().map_err(domain)?;
    snapshot.1 = false;
    candidates.retain(|candidate| candidate.confidence >= min_confidence);
    candidates.sort_by(|left, right| {
        right
            .confidence
            .total_cmp(&left.confidence)
            .then_with(|| right.session_arc.is_some().cmp(&left.session_arc.is_some()))
            .then_with(|| left.topic_key.cmp(&right.topic_key))
            .then_with(|| left.candidate_id.cmp(&right.candidate_id))
    });
    session_arc::limit_complete_pairs(candidates, usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(())
}

fn recorded(
    stored: &StoredCurationCandidate,
) -> Result<Option<ReviewCorroboration>, CurateValidationIssue> {
    let Some(raw) = stored.derivation_metadata_json.as_deref() else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(raw).map_err(issue)?;
    let Some(proof) = value.pointer("/producer/producerPayload/corroboration") else {
        return Ok(None);
    };
    serde_json::from_value(proof.clone())
        .map(Some)
        .map_err(issue)
}

/// Removing a support payload cannot turn an increment into an unproved
/// review-session score. Other derivation producers retain their own contract.
fn validate_unproved_review_confidence(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
) -> Result<(), CurateValidationIssue> {
    let Some(raw) = stored.derivation_metadata_json.as_deref() else {
        // The ordinary derivation validator diagnoses a wholly missing package.
        return Ok(());
    };
    let value: serde_json::Value = serde_json::from_str(raw).map_err(issue)?;
    if value
        .pointer("/producer/producer")
        .and_then(serde_json::Value::as_str)
        != Some("review_session")
    {
        return Ok(());
    }
    let metadata = parse_derivation_metadata(stored).map_err(|error| issue(error.message))?;
    let scores = [
        Some(stored.confidence),
        stored.proposed_confidence,
        metadata.memory_spec.confidence,
    ];
    if scores
        .into_iter()
        .flatten()
        .all(|score| score.is_finite() && score <= 0.6)
    {
        return Ok(());
    }
    if stored.confidence == 0.82
        && stored.proposed_confidence == Some(0.82)
        && metadata.memory_spec.confidence == Some(0.82)
        && value
            .pointer("/producer/producerPayload/sessionArc")
            .is_some()
    {
        // A JSON marker alone is not a legacy exemption. This reconstructs
        // the actual arc and compares its complete original canonical package.
        // The proofless branch of expected_for_recorded does not recurse here.
        return session_arc::applied_peer(connection, stored).map(|_| ());
    }
    Err(issue(
        "review-session confidence increment is missing its corroboration proof",
    ))
}

fn freeze_existing(
    connection: &DbConnection,
    workspace_id: &str,
    candidate: &mut ReviewSessionCandidate,
) -> Result<(), DomainError> {
    let Some(stored) = connection
        .get_curation_candidate(workspace_id, &candidate.candidate_id)
        .map_err(domain)?
    else {
        return Ok(());
    };
    let observed = candidate.confidence;
    candidate.confidence = stored.confidence;
    candidate.proposed_confidence = stored.proposed_confidence.unwrap_or(stored.confidence);
    candidate.reason = stored.reason.clone();
    candidate.candidate_type = stored.candidate_type.clone();
    candidate.target_memory_id = stored.target_memory_id.clone();
    if let Some(content) = &stored.proposed_content {
        candidate.proposed_content = content.clone();
        candidate.content_hash = content_hash_for_candidate(content);
    }
    candidate.source_ids = stored
        .source_id
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect();
    candidate.corroboration = recorded(&stored).map_err(|error| domain(error.message))?;
    if observed != candidate.confidence {
        candidate.reason.push_str(" Historical proposal: showing its recorded confidence and provenance; current corroboration does not rewrite it.");
    }
    Ok(())
}

/// Revalidate exact named sources in the caller's existing validation/write
/// transaction. New sessions cannot silently replace a disappeared supporter.
pub(super) fn validate_recorded(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
) -> Result<(), CurateValidationIssue> {
    let Some(proof) = recorded(stored)? else {
        return validate_unproved_review_confidence(connection, stored);
    };
    let expected = validate_proof(
        connection,
        &stored.workspace_id,
        &stored.id,
        stored.confidence,
        stored.proposed_confidence,
        &proof,
    )?;
    let (refs, metadata) =
        review_bootstrap_derivation_package(connection, &stored.workspace_id, &expected, None)
            .map_err(|error| issue(error.message()))?;
    if stored.candidate_type != expected.candidate_type
        || stored.target_memory_id != expected.target_memory_id
        || stored.source_type != persisted_review_candidate_source_type(&expected)
        || stored.source_id.as_deref() != Some(expected.source_ids.join(",").as_str())
        || stored.proposed_content.as_deref() != Some(expected.proposed_content.as_str())
        || stored.proposed_trust_class.is_some()
        || stored.derivation_source_refs_json.as_deref() != Some(refs.as_str())
        || stored.derivation_metadata_json.as_deref() != Some(metadata.as_str())
    {
        return Err(issue(
            "anchor content, type, ownership, or canonical source package changed",
        ));
    }
    Ok(())
}

pub(super) fn validate_draft(
    connection: &DbConnection,
    workspace_id: &str,
    candidate: &ReviewSessionCandidate,
) -> Result<(), CurateValidationIssue> {
    let Some(proof) = candidate
        .corroboration
        .as_ref()
        .filter(|proof| proof.recorded())
    else {
        if candidate.candidate_type == CandidateType::CreateDerivedMemory.as_str()
            && [candidate.confidence, candidate.proposed_confidence]
                .into_iter()
                .any(|score| !score.is_finite() || score > 0.6)
        {
            // This draft type is emitted only by the review-session producer.
            // Historical rows return before draft insertion and keep their
            // separate exact legacy validation path.
            return Err(issue(
                "new review-session confidence increment requires corroboration",
            ));
        }
        return Ok(());
    };
    let expected = validate_proof(
        connection,
        workspace_id,
        &candidate.candidate_id,
        candidate.confidence,
        Some(candidate.proposed_confidence),
        proof,
    )?;
    if !same_proposal(candidate, &expected) {
        return Err(issue(
            "proposal no longer matches the complete anchor source",
        ));
    }
    Ok(())
}

fn same_proposal(left: &ReviewSessionCandidate, right: &ReviewSessionCandidate) -> bool {
    left.candidate_id == right.candidate_id
        && left.candidate_type == right.candidate_type
        && left.candidate_kind == right.candidate_kind
        && left.topic_key == right.topic_key
        && left.target_memory_id == right.target_memory_id
        && left.proposed_content == right.proposed_content
        && left.content_hash == right.content_hash
        && left.source_ids == right.source_ids
        && persisted_review_candidate_source_type(left)
            == persisted_review_candidate_source_type(right)
        && left.session_arc == right.session_arc
}

fn validate_proof(
    connection: &DbConnection,
    workspace_id: &str,
    candidate_id: &str,
    confidence: f32,
    proposed_confidence: Option<f32>,
    proof: &ReviewCorroboration,
) -> Result<ReviewSessionCandidate, CurateValidationIssue> {
    if proof.schema != SCHEMA
        || proof.score_kind != SCORE_KIND
        || proof.sessions.is_empty()
        || proof.sessions.len() > MAX_SUPPORT
        || confidence != proof.confidence()
        || proposed_confidence != Some(proof.confidence())
        || proof.sessions.iter().any(|session| {
            session.sources.is_empty()
                || session.context_sources.is_empty()
                || session.sources.len() > LIMITS.spans_per_session as usize
                || session.context_sources.len() > LIMITS.spans_per_session as usize
        })
    {
        return Err(issue(
            "unsupported schema, support count, or heuristic score",
        ));
    }
    let ids = proof
        .sessions
        .iter()
        .map(|support| support.session_id.as_str())
        .collect::<Vec<_>>();
    let sessions = connection
        .review_corroboration_sessions(workspace_id, "", &ids, LIMITS)
        .map_err(issue)?;
    let mut verified = Vec::new();
    let mut anchor: Option<ReviewSessionCandidate> = None;
    for expected in &proof.sessions {
        let Some((session, spans, _)) = sessions
            .iter()
            .find(|(session, _, _)| session.id == expected.session_id)
        else {
            return Err(issue(
                "a supporting session is missing or exceeds the recorded proof bounds",
            ));
        };
        if !eligible_session(workspace_id, session, spans) {
            return Err(issue("support is no longer admitted or was refuted"));
        }
        let Some(candidate) = reconstruct_support(workspace_id, session, spans, expected) else {
            return Err(issue(
                "the recorded complete source lesson cannot be reconstructed",
            ));
        };
        if full_signature(&candidate, spans).as_ref() != Some(&proof.signature)
            || anchor
                .as_ref()
                .is_some_and(|anchor| anchor.candidate_kind != candidate.candidate_kind)
        {
            return Err(issue("the complete ordered lesson changed"));
        }
        // The outer curation lifecycle handles this proposal's own status.
        // Its immutable package remains inspectable after rejection (including
        // as the unaccepted peer of an independently accepted arc member).
        // A rejected additional observation cannot supply an increment.
        if !verified.is_empty()
            && rejected(connection, workspace_id, &candidate)
                .map_err(|error| issue(error.message()))?
        {
            return Err(issue("a supporting proposal was rejected"));
        }
        let Some(actual) = support_in_context(session, spans, &candidate, Some(expected)) else {
            return Err(issue("supporting source identity is missing"));
        };
        if actual != *expected || !independent(&actual, &verified) {
            return Err(issue("source identity, payload, or independence changed"));
        }
        verified.push(actual);
        if anchor.is_none() {
            anchor = Some(candidate);
        }
    }
    if proof
        .sessions
        .first()
        .is_none_or(|anchor| anchor.candidate_id != candidate_id)
    {
        return Err(issue("support does not belong to this anchor candidate"));
    }
    let mut anchor = anchor.ok_or_else(|| issue("missing anchor"))?;
    anchor.confidence = proof.confidence();
    anchor.proposed_confidence = proof.confidence();
    anchor.corroboration = Some(proof.clone());
    Ok(anchor)
}

/// Preserve exact historical packages while verifying new proofs rather than
/// accepting a caller-supplied confidence. Legacy 0.82 is a fixed producer
/// value, never an arbitrary unproved score for a new reconstruction.
pub(super) fn expected_for_recorded(
    connection: &DbConnection,
    stored: &StoredCurationCandidate,
    expected: &ReviewSessionCandidate,
) -> Result<ReviewSessionCandidate, CurateValidationIssue> {
    let mut expected = expected.clone();
    if let Some(proof) = recorded(stored)? {
        validate_recorded(connection, stored)?;
        expected.confidence = proof.confidence();
        expected.proposed_confidence = proof.confidence();
        expected.corroboration = Some(proof);
    } else {
        expected.corroboration = None;
        if stored.confidence == 0.82 && stored.proposed_confidence == Some(0.82) {
            expected.confidence = 0.82;
            expected.proposed_confidence = 0.82;
        }
    }
    Ok(expected)
}
