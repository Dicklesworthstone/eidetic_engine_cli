//! Public-evidence admission for extractive answers.
//!
//! Redacting an answer after segmentation would invalidate its stored byte
//! offsets. Withhold unsafe bodies before scoring instead; citation metadata
//! can be sanitized independently without changing the quoted evidence.

use std::path::Path;
use std::str::FromStr;

use crate::core::memory_scope::team_provenance_from_memory;
use crate::db::{DatabaseLocation, DbConnection, StoredMemory};
use crate::models::{DomainError, MemoryId, MemoryKind, MemoryLevel, ProvenanceUri, TrustClass};
use crate::policy::redact_public_replay_text;

use super::super::AskCandidate;

/// Team authority belongs to the workspace database, not an arbitrary alternate
/// store. A cross-store roster cannot join this evidence snapshot atomically;
/// withhold that query rather than silently widening or using stale membership.
pub(super) fn require_workspace_roster(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<(), DomainError> {
    let workspace = connection
        .get_workspace(workspace_id)
        .map_err(|_| super::corpus_storage_error())?
        .ok_or_else(super::corpus_storage_error)?;
    let expected = Path::new(&workspace.path).join(".ee").join("ee.db");
    let same_store = match connection.location() {
        DatabaseLocation::File(path) => path
            .canonicalize()
            .ok()
            .zip(expected.canonicalize().ok())
            .is_some_and(|(actual, expected)| actual == expected),
        DatabaseLocation::Memory => false,
    };
    if same_store {
        Ok(())
    } else {
        Err(DomainError::PolicyDenied {
            message: "Team-scoped ask requires the workspace database; no alternate-store roster was used".to_owned(),
            repair: Some("Run ee ask --memory-scope team without an alternate --database, or choose an explicitly non-team scope.".to_owned()),
        })
    }
}

pub(super) fn into_candidate(memory: StoredMemory) -> Option<AskCandidate> {
    let id = MemoryId::from_str(&memory.id).ok()?;
    let level = MemoryLevel::from_str(&memory.level).ok()?;
    let kind = MemoryKind::from_str(&memory.kind).ok()?;
    let trust = TrustClass::from_str(&memory.trust_class).ok()?;
    if memory.tombstoned_at.is_some()
        || memory.content.trim().is_empty()
        || memory.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
        || redact_public_replay_text(&memory.content).redacted
    {
        return None;
    }

    let fallback = || ProvenanceUri::EeMemory(id).to_string();
    let provenance_uri = memory
        .provenance_uri
        .as_deref()
        .filter(|uri| !redact_public_replay_text(uri).redacted)
        .and_then(|uri| ProvenanceUri::from_str(uri).ok())
        .map(|uri| uri.to_string())
        .unwrap_or_else(fallback);
    let mut team_provenance = team_provenance_from_memory(&memory);
    if let Some(team) = &mut team_provenance {
        team.member_display_name = redact_public_replay_text(&team.member_display_name).content;
        team.project_name = team
            .project_name
            .as_deref()
            .map(|name| redact_public_replay_text(name).content);
        team.produced_at = redact_public_replay_text(&team.produced_at).content;
    }
    Some(AskCandidate {
        memory_id: memory.id,
        content: memory.content,
        confidence: memory.confidence,
        trust_class: trust.as_str().to_owned(),
        provenance_uri: Some(provenance_uri),
        level: level.as_str().to_owned(),
        kind: kind.as_str().to_owned(),
        team_provenance,
    })
}
