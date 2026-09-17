//! Source-of-truth corpus admission for extractive question answering.
//!
//! A non-tombstoned row is not necessarily current: revision and expiration
//! retain old rows, and scheduled memories can start in the future. Apply the
//! validity window before scoring, nearest-evidence hints, and link lookup so
//! none of those paths can bring ineligible advice back into an answer.

use chrono::{DateTime, Utc};

use crate::db::DbConnection;
use crate::models::DomainError;

use super::{AskCandidate, AskContradiction, load_scoped_contradictions};

#[path = "ask_admission.rs"]
mod admission;

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
/// offending body, identifier, or timestamp in the error.
///
/// Memory bodies, validity metadata, and every batch of incident links come
/// from one database read snapshot. The snapshot is released before returning
/// owned data for scoring or best-effort audit writes. No writer fence,
/// migrations, index/model loading, or cross-workspace expansion occur here.
pub fn load_current_ask_corpus(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
) -> Result<AskCorpus, DomainError> {
    load_corpus_with_boundary(connection, workspace_id, reference_time, || Ok(()))
}

// The private boundary lets real-store tests commit through a second connection
// at the exact memory/link boundary. Production passes a no-op, not a timing
// sleep or a mock database. The owned snapshot encloses both reads regardless.
fn load_corpus_with_boundary(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
    after_memory_read: impl FnOnce() -> Result<(), DomainError>,
) -> Result<AskCorpus, DomainError> {
    let snapshot = AskReadSnapshot::begin(connection)?;
    let stored = connection
        .list_memories(workspace_id, None, false)
        .map_err(|_| corpus_storage_error())?;
    after_memory_read()?;
    let mut candidates = Vec::with_capacity(stored.len());
    for memory in stored {
        if validity_contains(
            memory.valid_from.as_deref(),
            memory.valid_to.as_deref(),
            reference_time,
        )? && let Some(candidate) = admission::into_candidate(memory)
        {
            candidates.push(candidate);
        }
    }
    let ids: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.memory_id.as_str())
        .collect();
    let contradictions = load_scoped_contradictions(connection, &ids)?;
    snapshot.finish()?;
    Ok(AskCorpus {
        candidates,
        contradictions,
    })
}

/// Own only the read transaction that this operation successfully began.
/// A failed nested begin must never roll back a caller's existing transaction.
/// Errors and unwinding release our snapshot; a failed commit is rolled back
/// rather than leaving the connection pinned for later audit writes.
struct AskReadSnapshot<'a> {
    connection: &'a DbConnection,
    active: bool,
}

impl<'a> AskReadSnapshot<'a> {
    fn begin(connection: &'a DbConnection) -> Result<Self, DomainError> {
        connection
            .begin_read_snapshot()
            .map_err(|_| snapshot_error("begin"))?;
        Ok(Self {
            connection,
            active: true,
        })
    }

    fn finish(mut self) -> Result<(), DomainError> {
        self.connection
            .commit_read_snapshot()
            .map_err(|_| snapshot_error("finish"))?;
        self.active = false;
        Ok(())
    }
}

impl Drop for AskReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.connection.rollback_read_snapshot().is_err() {
            // Do not echo backend errors that may contain SQL or private paths.
            tracing::error!(
                target: "ee::core::ask::snapshot",
                "failed to release ask evidence read snapshot"
            );
        }
    }
}

fn snapshot_error(stage: &str) -> DomainError {
    DomainError::Storage {
        message: format!("Could not {stage} a coherent ask evidence snapshot; answer withheld"),
        repair: Some("retry ee ask; use ee doctor --json if the failure persists".to_owned()),
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
    Ok(from.is_none_or(|from| from <= reference_time) && to.is_none_or(|to| reference_time <= to))
}

#[cfg(test)]
#[path = "ask_corpus_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ask_snapshot_tests.rs"]
mod snapshot_tests;

#[cfg(test)]
#[path = "ask_privacy_tests.rs"]
mod privacy_tests;
