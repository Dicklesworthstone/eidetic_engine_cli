//! Source-of-truth corpus admission for extractive question answering.
//!
//! Current lifecycle, scope and public-evidence eligibility are resolved before
//! scoring, nearest-evidence hints, and incident-link lookup. Memory bodies,
//! scope metadata and links must describe one coherent database snapshot.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use crate::core::memory_scope::MemoryScopeContext;
use crate::db::DbConnection;
use crate::models::{DomainError, MemoryScope};

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

/// Apply the ordinary memory scope before scoring or contradiction lookup.
/// Global scope selects tagged memories in this workspace; it never opens a
/// global store or widens the already-resolved workspace boundary.
pub fn load_scoped_ask_corpus(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
    scope: MemoryScope,
) -> Result<AskCorpus, DomainError> {
    load_corpus_with_scope_boundary(
        connection,
        workspace_id,
        reference_time,
        || scope_context(connection, workspace_id, scope),
        || Ok(()),
    )
}

fn scope_context(
    connection: &DbConnection,
    workspace_id: &str,
    scope: MemoryScope,
) -> Result<MemoryScopeContext, DomainError> {
    let mut context = MemoryScopeContext {
        scope,
        strict_scope: false,
        current_agent: crate::core::memory_scope::current_agent_name(),
        team_members: BTreeSet::new(),
    };
    if scope == MemoryScope::Team {
        admission::require_workspace_roster(connection, workspace_id)?;
        // The addressed store's authenticated roster is authority, not a
        // config-file list or a roster from a different workspace/database.
        for member in connection
            .list_all_team_members()
            .map_err(|_| corpus_storage_error())?
        {
            if member.workspace_id == workspace_id && member.state == "active" {
                for name in [member.display_name, member.origin_node_id] {
                    let name = name.trim();
                    if !name.is_empty() {
                        context.team_members.insert(name.to_owned());
                    }
                }
            }
        }
    }
    Ok(context)
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
    load_corpus_with_scope_boundary(
        connection,
        workspace_id,
        reference_time,
        || {
            Ok(MemoryScopeContext {
                scope: MemoryScope::Workspace,
                strict_scope: false,
                current_agent: None,
                team_members: BTreeSet::new(),
            })
        },
        after_memory_read,
    )
}

fn load_corpus_with_scope_boundary(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
    scope_context: impl FnOnce() -> Result<MemoryScopeContext, DomainError>,
    after_memory_read: impl FnOnce() -> Result<(), DomainError>,
) -> Result<AskCorpus, DomainError> {
    let snapshot = AskReadSnapshot::begin(connection)?;
    let stored = connection
        .list_memories(workspace_id, None, false)
        .map_err(|_| corpus_storage_error())?;
    let scope = scope_context()?;
    after_memory_read()?;
    let mut tags = std::collections::BTreeMap::new();
    if scope.scope == MemoryScope::Global {
        let ids: Vec<_> = stored.iter().map(|memory| memory.id.as_str()).collect();
        for batch in ids.chunks(256) {
            tags.extend(
                connection
                    .get_memory_tags_batch(batch)
                    .map_err(|_| corpus_storage_error())?,
            );
        }
    }
    let mut candidates = Vec::with_capacity(stored.len());
    for memory in stored {
        if validity_contains(
            memory.valid_from.as_deref(),
            memory.valid_to.as_deref(),
            reference_time,
        )? && memory.workspace_id == workspace_id
            && scope.memory_in_scope_with_tags(
                &memory,
                tags.get(&memory.id).map(Vec::as_slice).unwrap_or(&[]),
            )
            && let Some(candidate) = admission::into_candidate(memory)
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

#[cfg(test)]
#[path = "ask_scope_tests.rs"]
mod scope_tests;
