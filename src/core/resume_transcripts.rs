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
    const WINDOW: usize = crate::policy::MAX_PUBLIC_REPLAY_TEXT_SCAN_BYTES;
    if excerpt.len() > crate::models::MAX_CONTENT_BYTES
        || excerpt
            .char_indices()
            .any(|(index, _)| crate::util::sensitive_path_starts_at(excerpt, index))
    {
        // The shared replay detector skips URI slashes at bare-path boundaries.
        // file:///home/... is still a private path, not a public transcript.
        return false;
    }
    if excerpt.len() > WINDOW {
        let screened = crate::policy::screen_external_text_for_ingestion(excerpt);
        if screened.redacted
            || screened.instruction_like
            || excerpt.split_whitespace().any(|atom| atom.len() > WINDOW / 4)
        {
            return false;
        }
    }
    let mut start = 0;
    loop {
        let mut end = excerpt.len().min(start + WINDOW);
        while !excerpt.is_char_boundary(end) {
            end -= 1;
        }
        if crate::policy::redact_public_replay_text(&excerpt[start..end]).redacted {
            return false;
        }
        if end == excerpt.len() {
            return true;
        }
        start += WINDOW / 2;
        while !excerpt.is_char_boundary(start) {
            start += 1;
        }
    }
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
        || !public_excerpt(&span.excerpt)
    {
        return None;
    }
    let provenance_uri = span.canonical_provenance_uri();
    if crate::policy::redact_public_replay_text(&provenance_uri).redacted {
        return None;
    }
    let mut end = span.excerpt.len().min(TRANSCRIPT_CONTENT_BYTE_CAP);
    while !span.excerpt.is_char_boundary(end) {
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
        content: span.excerpt[..end].to_owned(),
        content_byte_start: 0,
        content_byte_end: end,
        excerpt_bytes: span.excerpt.len(),
        content_truncated: end < span.excerpt.len(),
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
    connection
        .visit_search_admitted_evidence_spans_in_current_snapshot(workspace_id, |span| {
            // A linked excerpt cannot resurrect a sealed, retired, foreign or
            // otherwise excluded memory via a new evidence identity.
            if span.workspace_id != workspace_id
                || span
                    .memory_id
                    .as_deref()
                    .is_some_and(|id| !admitted_memories.contains(id))
            {
                return Ok(());
            }
            let Some(item) = admitted_item(&span) else {
                return Ok(());
            };
            let Some(header) = headers.get(&span.session_id) else {
                inconsistent_session = true;
                return Ok(());
            };
            *counts.entry(span.session_id.clone()).or_default() += 1;
            if limit == 0 {
                return Ok(());
            }
            let rank = (Reverse(header.activity()), span.session_id.clone());
            if !selected.contains_key(&rank) {
                if selected.len() == limit {
                    if selected
                        .last_key_value()
                        .is_some_and(|(worst, _)| &rank >= worst)
                    {
                        return Ok(());
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
            Ok(())
        })
        .map_err(|_| storage_error())?;
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
