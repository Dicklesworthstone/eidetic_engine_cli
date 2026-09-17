//! Source-of-truth corpus admission for extractive question answering.
//!
//! A non-tombstoned row is not necessarily current: revision and expiration
//! retain old rows, and scheduled memories can start in the future. Apply the
//! validity window before scoring, nearest-evidence hints, and link lookup so
//! none of those paths can bring ineligible advice back into an answer.

use chrono::{DateTime, Utc};

use crate::db::{DbConnection, StoredMemory};
use crate::models::DomainError;

use super::{AskCandidate, AskContradiction, load_scoped_contradictions};

#[derive(Clone, Debug)]
pub struct AskCorpus {
    pub candidates: Vec<AskCandidate>,
    pub contradictions: Vec<AskContradiction>,
}

/// Load current evidence for one already-resolved workspace.
///
/// `reference_time` is captured once by the caller, not separately per row.
/// Bounds are inclusive, matching search's validity-window contract. Invalid
/// timestamps or inverted windows fail the entire read without exposing the
/// offending body, identifier, or timestamp in the error. No writes, migrations,
/// index/model loading, or cross-workspace expansion occur here.
pub fn load_current_ask_corpus(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
) -> Result<AskCorpus, DomainError> {
    let stored = connection
        .list_memories(workspace_id, None, false)
        .map_err(|_| corpus_storage_error())?;
    let mut candidates = Vec::with_capacity(stored.len());
    for memory in stored {
        if validity_contains(
            memory.valid_from.as_deref(),
            memory.valid_to.as_deref(),
            reference_time,
        )? {
            candidates.push(into_candidate(memory));
        }
    }
    let ids: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.memory_id.as_str())
        .collect();
    let contradictions = load_scoped_contradictions(connection, &ids)?;
    Ok(AskCorpus {
        candidates,
        contradictions,
    })
}

fn into_candidate(memory: StoredMemory) -> AskCandidate {
    let team_provenance = crate::core::memory_scope::team_provenance_from_memory(&memory);
    AskCandidate {
        memory_id: memory.id,
        content: memory.content,
        confidence: memory.confidence,
        trust_class: memory.trust_class,
        provenance_uri: memory.provenance_uri,
        level: memory.level,
        kind: memory.kind,
        team_provenance,
    }
}

fn corpus_storage_error() -> DomainError {
    DomainError::Storage {
        message: "Failed to read the ask evidence corpus".to_owned(),
        repair: Some("ee doctor --json".to_owned()),
    }
}

fn invalid_validity_error() -> DomainError {
    DomainError::Storage {
        message: "Ask evidence contains invalid validity metadata; answer withheld".to_owned(),
        repair: Some("ee doctor --json".to_owned()),
    }
}

fn validity_contains(
    valid_from: Option<&str>,
    valid_to: Option<&str>,
    reference_time: DateTime<Utc>,
) -> Result<bool, DomainError> {
    let parse = |raw: &str| {
        DateTime::parse_from_rfc3339(raw)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|_| invalid_validity_error())
    };
    // Parse both bounds before testing visibility. An already-expired or
    // not-yet-active bound cannot hide malformed metadata in the other bound.
    let from = valid_from.map(parse).transpose()?;
    let to = valid_to.map(parse).transpose()?;
    if let (Some(from), Some(to)) = (from, to)
        && from > to
    {
        return Err(invalid_validity_error());
    }
    Ok(from.is_none_or(|from| from <= reference_time)
        && to.is_none_or(|to| reference_time <= to))
}

#[cfg(test)]
#[path = "ask_corpus_tests.rs"]
mod tests;
