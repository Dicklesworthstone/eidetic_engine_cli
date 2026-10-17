//! Native, positively admitted transcript history for the read-only resume.
//!
//! A transcript is historical evidence, not a curated decision or a newly
//! inferred open loop. Preserve its real session/evidence IDs and revision.
//! Never open a source path, rebuild an index, initialize keys, or consult a
//! different workspace. The caller owns the snapshot enclosing these reads.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use sqlmodel_core::Value;

use super::{RESUME_SESSION_CAP, RESUME_STORAGE_PAGE_SIZE, SESSION_ITEM_CAP};
use crate::core::incident_card::proof::{CardChecks, requires_proof};
use crate::db::{DbConnection, StoredEvidenceSpan, StoredMemory};
use crate::models::{DomainError, EvidenceId, SessionId, TrustClass};

/// Maximum public prefix of one admitted excerpt, measured in UTF-8 bytes.
/// Admission and privacy screening always inspect the complete stored excerpt.
pub const TRANSCRIPT_CONTENT_BYTE_CAP: usize = 4096;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeTranscriptHistory {
    /// Counts only sessions with at least one public, currently admitted span.
    pub session_total: usize,
    pub evidence_total: usize,
    pub sessions_truncated: bool,
    /// Source activity descending; unknown dates last, then canonical ID.
    pub sessions: Vec<ResumeTranscriptSession>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeTranscriptSession {
    pub session_id: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub activity_at: Option<String>,
    /// A source timestamp, never the wall clock or an inferred import time.
    pub activity_basis: &'static str,
    pub admitted_span_count: usize,
    pub items_truncated: bool,
    /// End-of-session line windows first; every item retains its source range.
    pub items: Vec<ResumeTranscriptItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeTranscriptItem {
    pub evidence_id: String,
    pub entity_kind: &'static str,
    pub entity_revision: String,
    pub provenance_uri: String,
    pub trust_class: String,
    pub selection_reason: &'static str,
    pub start_line: u64,
    pub end_line: u64,
    /// Byte-exact prefix of the stored excerpt, not generated advice.
    pub content: String,
    pub content_byte_start: usize,
    pub content_byte_end: usize,
    pub excerpt_bytes: usize,
    pub content_truncated: bool,
}

type SessionRank = (Reverse<Option<DateTime<Utc>>>, String);

#[derive(Clone)]
struct SessionHeader {
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
}

impl SessionHeader {
    fn activity(&self) -> Option<DateTime<Utc>> {
        self.ended_at.or(self.started_at)
    }

    fn into_session(self, id: String) -> ResumeTranscriptSession {
        let format = |time: DateTime<Utc>| time.to_rfc3339_opts(SecondsFormat::AutoSi, true);
        ResumeTranscriptSession {
            session_id: id,
            activity_at: self.activity().map(format),
            activity_basis: if self.ended_at.is_some() {
                "source_ended_at"
            } else if self.started_at.is_some() {
                "source_started_at"
            } else {
                "unknown"
            },
            started_at: self.started_at.map(format),
            ended_at: self.ended_at.map(format),
            admitted_span_count: 0,
            items_truncated: false,
            items: Vec::new(),
        }
    }
}

fn storage_error() -> DomainError {
    // Stored transcript text, opaque upstream IDs and source paths must not
    // become diagnostics when a row or the storage operation is malformed.
    super::resume_storage_error(
        "Could not read coherent admitted transcript history; resume withheld",
    )
}

fn optional_timestamp(value: Option<&Value>) -> Result<Option<DateTime<Utc>>, DomainError> {
    match value {
        Some(Value::Null) => Ok(None),
        Some(Value::Text(raw)) => super::parse_ts(raw).map(Some).ok_or_else(storage_error),
        _ => Err(storage_error()),
    }
}

/// Page only lightweight session identity/chronology, not private metadata or
/// transcript bodies. A page size limits SQL result allocation, not history.
fn session_headers(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<BTreeMap<String, SessionHeader>, DomainError> {
    let mut headers = BTreeMap::new();
    let mut after = String::new();
    loop {
        let rows = connection
            .query(
                "SELECT id, started_at, ended_at FROM sessions WHERE workspace_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3",
                &[
                    Value::Text(workspace_id.to_owned()),
                    Value::Text(after.clone()),
                    Value::BigInt(RESUME_STORAGE_PAGE_SIZE as i64),
                ],
            )
            .map_err(|_| storage_error())?;
        let count = rows.len();
        for row in rows {
            let Some(Value::Text(id)) = row.get(0) else {
                return Err(storage_error());
            };
            if id <= &after
                || !SessionId::from_str(id).is_ok_and(|parsed| parsed.to_string() == *id)
            {
                return Err(storage_error());
            }
            let started_at = optional_timestamp(row.get(1))?;
            let ended_at = optional_timestamp(row.get(2))?;
            if started_at
                .zip(ended_at)
                .is_some_and(|(start, end)| end < start)
            {
                return Err(storage_error());
            }
            after = id.clone();
            headers.insert(
                id.clone(),
                SessionHeader {
                    started_at,
                    ended_at,
                },
            );
        }
        if count < RESUME_STORAGE_PAGE_SIZE {
            break;
        }
    }
    Ok(headers)
}

fn line_number<T: TryInto<u64>>(value: T) -> Option<u64> {
    value.try_into().ok()
}

/// Screen the complete admitted source before publishing even a short prefix.
/// Ingestion permits source paths and records instruction risk; neither grants
/// public replay authority. Full-input screening is needed in addition to
/// overlapping windows: contextual credentials and instruction phrases can
/// contain arbitrarily wide whitespace and token detectors need intact atoms.
fn public_excerpt(excerpt: &str) -> bool {
    !crate::policy::redact_public_replay_body(excerpt).redacted
}

#[cfg(test)]
#[path = "resume_transcript_privacy_tests.rs"]
mod privacy_tests;

fn admitted_item(span: &StoredEvidenceSpan) -> Option<ResumeTranscriptItem> {
    if !EvidenceId::from_str(&span.id).is_ok_and(|id| id.to_string() == span.id)
        || !SessionId::from_str(&span.session_id).is_ok_and(|id| id.to_string() == span.session_id)
        || span.start_line == 0
        || span.end_line < span.start_line
        || span.excerpt.trim().is_empty()
        || span.excerpt == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
    {
        return None;
    }
    // Publish and screen the projected message body, not the transcript
    // envelope (bd-reality-core-convergence-1azkt.45). Screening the envelope
    // withheld every Claude Code record, whose `cwd` is an absolute path; a
    // plain excerpt projects to itself and is screened exactly as before.
    let text = span.reader_text();
    if text.trim().is_empty() || !public_excerpt(&text) {
        return None;
    }
    // The URI is minted from the validated native session id and line range,
    // never from transcript text. Screening it as free text refused every real
    // span: a random ULID reads as a high-entropy token.
    let provenance_uri = span.canonical_provenance_uri();
    let mut end = text.len().min(TRANSCRIPT_CONTENT_BYTE_CAP);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Some(ResumeTranscriptItem {
        evidence_id: span.id.clone(),
        entity_kind: "evidence_span",
        entity_revision: span.pack_entity_revision(),
        provenance_uri,
        trust_class: TrustClass::CassEvidence.as_str().to_owned(),
        selection_reason: "recent_admitted_transcript",
        start_line: line_number(span.start_line)?,
        end_line: line_number(span.end_line)?,
        content: text[..end].to_owned(),
        content_byte_start: 0,
        content_byte_end: end,
        excerpt_bytes: text.len(),
        content_truncated: end < text.len(),
    })
}

fn retain_tail(session: &mut ResumeTranscriptSession, item: ResumeTranscriptItem) {
    session.items.push(item);
    session.items.sort_by(|left, right| {
        right
            .end_line
            .cmp(&left.end_line)
            .then_with(|| right.start_line.cmp(&left.start_line))
            .then_with(|| left.evidence_id.cmp(&right.evidence_id))
    });
    session.items.truncate(SESSION_ITEM_CAP);
}

pub(super) fn load(
    connection: &DbConnection,
    workspace_id: &str,
    all_live: &[StoredMemory],
    limit: usize,
) -> Result<ResumeTranscriptHistory, DomainError> {
    let headers = session_headers(connection, workspace_id)?;
    // Memory-only stores must not require or initialize transcript authority.
    if headers.is_empty() {
        return Ok(ResumeTranscriptHistory::default());
    }
    let admitted_memories: BTreeSet<_> = all_live.iter().map(|memory| memory.id.as_str()).collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut selected: BTreeMap<SessionRank, ResumeTranscriptSession> = BTreeMap::new();
    let limit = limit.min(RESUME_SESSION_CAP);
    let mut inconsistent_session = false;
    // A linked excerpt cannot resurrect a sealed, retired, foreign or
    // otherwise excluded memory via a new evidence identity.
    let in_scope = |span: &StoredEvidenceSpan| {
        span.workspace_id == workspace_id
            && span.memory_id.as_deref().is_none_or(|id| admitted_memories.contains(id))
    };
    {
        // Both ordinary sources and currently proved cards pass the same
        // public-prefix policy and bounded reduction. Refused cards never
        // affect counts, session selection, tail slots or truncation flags.
        let mut retain = |span: &StoredEvidenceSpan| {
            if !in_scope(span) {
                return;
            }
            let Some(item) = admitted_item(span) else {
                return;
            };
            let Some(header) = headers.get(&span.session_id) else {
                inconsistent_session = true;
                return;
            };
            *counts.entry(span.session_id.clone()).or_default() += 1;
            if limit == 0 {
                return;
            }
            let rank = (Reverse(header.activity()), span.session_id.clone());
            if !selected.contains_key(&rank) {
                if selected.len() == limit {
                    if selected
                        .last_key_value()
                        .is_some_and(|(worst, _)| &rank >= worst)
                    {
                        return;
                    }
                    selected.pop_last();
                }
                selected.insert(
                    rank.clone(),
                    header.clone().into_session(span.session_id.clone()),
                );
            }
            if let Some(session) = selected.get_mut(&rank) {
                retain_tail(session, item);
            }
        };
        let mut cards = CardChecks::default();
        connection
            .visit_search_admitted_evidence_spans_in_current_snapshot(workspace_id, |span| {
                if !in_scope(&span) {
                    return Ok(());
                }
                if requires_proof(&span) {
                    // Retain only IDs and commitments while visiting sources.
                    // Altered reserved locators cannot fall through as prose.
                    cards.observe(&span)?;
                } else {
                    retain(&span);
                }
                Ok(())
            })
            .map_err(|_| storage_error())?;
        cards
            .visit_verified(connection, workspace_id, |span| {
                retain(&span);
                Ok(())
            })
            .map_err(|_| storage_error())?;
    }
    if inconsistent_session {
        return Err(storage_error());
    }
    let session_total = counts.len();
    let evidence_total = counts.values().sum();
    let sessions: Vec<_> = selected
        .into_values()
        .map(|mut session| {
            session.admitted_span_count = counts[&session.session_id];
            session.items_truncated = session.admitted_span_count > session.items.len();
            session
        })
        .collect();
    Ok(ResumeTranscriptHistory {
        session_total,
        evidence_total,
        sessions_truncated: session_total > sessions.len(),
        sessions,
    })
}

#[cfg(test)]
mod incident_proof_tests {
    use super::*;
    use crate::core::incident_card::proof::tests::{Fixture, TestResult, WS};
    use crate::db::{CreateEvidenceSpanInput, CreateSessionInput, EvidenceProducerKind};

    fn history(fixture: &Fixture, limit: usize) -> TestResult<ResumeTranscriptHistory> {
        fixture.db.begin_read_snapshot()?;
        let result = load(&fixture.db, WS, &[], limit);
        // Successful and failed loaders must leave snapshot ownership here.
        fixture.db.rollback_read_snapshot()?;
        Ok(result?)
    }

    fn item_ids(history: &ResumeTranscriptHistory) -> BTreeSet<&str> {
        history.sessions.iter().flat_map(|session| {
            session.items.iter().map(|item| item.evidence_id.as_str())
        }).collect()
    }

    fn assert_candidate(fixture: &Fixture, id: &str) -> TestResult {
        let mut seen = false;
        fixture.db.begin_read_snapshot()?;
        let result = fixture.db.visit_search_admitted_evidence_spans_in_current_snapshot(WS, |span| {
            if span.id == id {
                seen = true;
                assert!(admitted_item(&span).is_some(), "public reader candidate required");
            }
            Ok(())
        });
        fixture.db.rollback_read_snapshot()?;
        result?;
        assert!(seen, "fixture must pass the original source-admission boundary");
        Ok(())
    }

    fn insert_copy(
        fixture: &Fixture,
        card: &StoredEvidenceSpan,
        number: u128,
        session: &str,
        line: u32,
    ) -> TestResult<String> {
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        fixture.db.insert_evidence_span(&id, &CreateEvidenceSpanInput {
            workspace_id: WS.to_owned(),
            session_id: session.to_owned(),
            memory_id: None,
            producer_kind: EvidenceProducerKind::parse(&card.producer_kind).ok_or("producer")?,
            cass_span_id: format!("ee-incident-card:resume-test:{number}"),
            span_kind: card.span_kind.clone(),
            start_line: line,
            end_line: line,
            start_byte: None,
            end_byte: None,
            role: card.role.clone(),
            content_hash: card.content_hash.clone(),
            excerpt: card.excerpt.clone(),
            metadata_json: card.metadata_json.clone(),
            inherited_redaction_classes: Vec::new(),
        })?;
        Ok(id)
    }

    #[test]
    fn historical_unproved_card_cannot_change_resume_counts_or_output() -> TestResult {
        let fixture = Fixture::new(false)?;
        let before = history(&fixture, RESUME_SESSION_CAP)?;
        assert!(item_ids(&before).contains(fixture.repair.as_str()));
        let id = fixture.old_card()?;
        assert_candidate(&fixture, &id)?;
        let card = fixture.db.get_evidence_span(&id)?.ok_or("card")?;
        assert_eq!(history(&fixture, RESUME_SESSION_CAP)?, before);
        let after = fixture.db.get_evidence_span(&id)?.ok_or("history must remain stored")?;
        assert_eq!(after.excerpt, card.excerpt);
        assert_eq!(after.content_hash, card.content_hash);
        assert_eq!(after.metadata_json, card.metadata_json);
        assert_eq!(after.canonical_provenance_uri(), card.canonical_provenance_uri());
        Ok(())
    }

    #[test]
    fn valid_card_preserves_exact_admitted_item_and_is_counted_once() -> TestResult {
        let fixture = Fixture::new(true)?;
        let before = history(&fixture, RESUME_SESSION_CAP)?;
        assert!(item_ids(&before).contains(fixture.repair.as_str()));
        let card = fixture.current_card()?;
        assert_candidate(&fixture, &card.id)?;
        let expected = admitted_item(&card).ok_or("public current card")?;
        let result = history(&fixture, RESUME_SESSION_CAP)?;
        let items: Vec<_> = result.sessions.iter().flat_map(|session| &session.items)
            .filter(|item| item.evidence_id == card.id).collect();
        assert_eq!(items, vec![&expected]);
        assert_eq!(result.evidence_total, before.evidence_total + 1);
        assert_eq!(result.session_total, before.session_total);
        assert_eq!(history(&fixture, RESUME_SESSION_CAP)?, result);
        Ok(())
    }

    #[test]
    fn changed_current_proof_withholds_only_derived_guidance_and_preserves_stored_card() -> TestResult {
        let fixture = Fixture::new(true)?;
        let before = history(&fixture, RESUME_SESSION_CAP)?;
        let card = fixture.current_card()?;
        assert!(item_ids(&history(&fixture, RESUME_SESSION_CAP)?).contains(card.id.as_str()));
        fixture.db.execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = 'No execution proof remains' WHERE id = '{}'",
            fixture.proof,
        ))?;
        assert_candidate(&fixture, &card.id)?;
        let result = history(&fixture, RESUME_SESSION_CAP)?;
        assert!(!item_ids(&result).contains(card.id.as_str()));
        assert!(item_ids(&result).contains(fixture.repair.as_str()));
        assert_eq!(result, before);
        assert_eq!(fixture.db.get_evidence_span(&card.id)?.ok_or("card retained")?.excerpt, card.excerpt);
        Ok(())
    }

    #[test]
    fn rejected_tail_cards_cannot_crowd_valid_transcript_out_of_the_item_cap() -> TestResult {
        let fixture = Fixture::new(false)?;
        let before = history(&fixture, RESUME_SESSION_CAP)?;
        assert!(!before.sessions.is_empty());
        assert!(item_ids(&before).contains(fixture.repair.as_str()));
        let id = fixture.old_card()?;
        let card = fixture.db.get_evidence_span(&id)?.ok_or("card")?;
        assert_candidate(&fixture, &id)?;
        for number in 0..SESSION_ITEM_CAP + 2 {
            let copy = insert_copy(&fixture, &card, 0x9000 + number as u128,
                &fixture.session, 100 + u32::try_from(number)?)?;
            assert_candidate(&fixture, &copy)?;
        }
        assert_eq!(history(&fixture, RESUME_SESSION_CAP)?, before);
        Ok(())
    }

    #[test]
    fn a_newer_session_containing_only_unproved_cards_cannot_consume_a_session_slot() -> TestResult {
        let fixture = Fixture::new(false)?;
        let before = history(&fixture, 1)?;
        assert_eq!(before.sessions.len(), 1);
        let id = fixture.old_card()?;
        let card = fixture.db.get_evidence_span(&id)?.ok_or("card")?;
        let session = SessionId::from_uuid(uuid::Uuid::from_u128(0x9800)).to_string();
        fixture.db.insert_session(&session, &CreateSessionInput {
            workspace_id: WS.to_owned(), cass_session_id: "newer-unproved-resume".to_owned(),
            source_path: None, agent_name: Some("claude_code".to_owned()), model: None,
            started_at: Some("2026-10-17T12:00:00Z".to_owned()),
            ended_at: Some("2026-10-17T12:01:00Z".to_owned()), message_count: 1,
            token_count: None, content_hash: card.content_hash.clone(), metadata_json: None,
        })?;
        let copy = insert_copy(&fixture, &card, 0x9900, &session, 100)?;
        assert_candidate(&fixture, &copy)?;
        assert_eq!(history(&fixture, 1)?, before);
        Ok(())
    }

    #[test]
    fn zero_display_limit_still_counts_only_currently_proved_cards() -> TestResult {
        for valid in [false, true] {
            let fixture = Fixture::new(valid)?;
            let before = history(&fixture, 0)?;
            assert!(before.evidence_total > 0);
            if valid {
                fixture.current_card()?;
            } else {
                fixture.old_card()?;
            }
            let result = history(&fixture, 0)?;
            assert!(result.sessions.is_empty());
            assert_eq!(result.evidence_total, before.evidence_total + usize::from(valid));
            assert_eq!(result.session_total, before.session_total);
            assert!(result.sessions_truncated);
        }
        Ok(())
    }

    #[test]
    fn a_current_proof_cannot_restore_a_card_with_denied_source_admission() -> TestResult {
        let fixture = Fixture::new(true)?;
        let before = history(&fixture, RESUME_SESSION_CAP)?;
        let card = fixture.current_card()?;
        assert_candidate(&fixture, &card.id)?;
        fixture.db.execute_raw(&format!(
            "UPDATE evidence_spans SET search_eligibility = 'denied', pack_eligibility = 'denied' WHERE id = '{}'",
            card.id,
        ))?;
        // The historical execution remains proved, but that does not grant
        // today's source-admission authority or public replay permission.
        let proofs = crate::core::incident_card::proof::session_proofs(
            &fixture.db, WS, None, &fixture.session,
        )?;
        assert!(proofs.claims.iter().any(|claim| claim.card && claim.target == card.id));
        assert_eq!(history(&fixture, RESUME_SESSION_CAP)?, before);
        let stored = fixture.db.get_evidence_span(&card.id)?.ok_or("denied card retained")?;
        assert_eq!(stored.excerpt, card.excerpt);
        Ok(())
    }

    #[test]
    fn over_budget_proof_keeps_ordinary_history_without_partial_card_admission() -> TestResult {
        let fixture = Fixture::new(true)?;
        let before = history(&fixture, RESUME_SESSION_CAP)?;
        let card = fixture.current_card()?;
        assert_candidate(&fixture, &card.id)?;
        fixture.db.execute_raw(&format!(
            "UPDATE evidence_spans SET metadata_json = '{}' WHERE id = '{}'",
            "x".repeat(crate::core::incident_card::proof::MAX_SOURCE_BYTES), fixture.failure,
        ))?;
        assert_eq!(history(&fixture, RESUME_SESSION_CAP)?, before);
        assert_eq!(fixture.db.get_evidence_span(&card.id)?.ok_or("card retained")?.excerpt, card.excerpt);
        Ok(())
    }
}
