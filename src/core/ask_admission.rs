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

pub(super) fn rule_candidate(
    projection: &crate::search::RuleIndexProjection,
) -> Option<(AskCandidate, super::super::AskNativeSource)> {
    let rule = projection.rule();
    let id = crate::models::RuleId::from_str(&rule.id).ok()?;
    let trust = TrustClass::from_str(&rule.trust_class).ok()?;
    if !projection.is_pack_admissible()
        || rule.content.trim().is_empty()
        || rule.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
        || !public_text(&rule.content)
    {
        return None;
    }
    let entity = crate::pack::PackEntityRef::Rule(id);
    Some((
        AskCandidate {
            memory_id: rule.id.clone(),
            content: rule.content.clone(),
            confidence: rule.confidence,
            trust_class: trust.as_str().to_owned(),
            provenance_uri: Some(entity.provenance_uri()),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            team_provenance: None,
        },
        super::super::AskNativeSource {
            entity,
            entity_revision: projection.entity_revision().to_owned(),
            source_memory_ids: projection.source_memory_ids().to_vec(),
        },
    ))
}

/// Imported transcripts are first-class answer sources, not synthetic memories.
/// Called only inside the corpus owner's read snapshot: bodies, session
/// admission, revisions and optional derivation links must describe that same
/// snapshot. Neither a stale search index nor a linked memory grants authority.
pub(super) fn append_evidence(
    connection: &DbConnection,
    workspace_id: &str,
    scope: crate::models::MemoryScope,
    candidates: &mut Vec<AskCandidate>,
    native_sources: &mut std::collections::BTreeMap<String, super::super::AskNativeSource>,
) -> Result<(), DomainError> {
    use crate::models::{EvidenceId, MemoryScope};

    // Raw CASS evidence has no authenticated agent membership, global tag or
    // verification attestation. Inheriting those from a distilled memory would
    // widen self/team/global/verified scope and launder the transcript's trust.
    if !matches!(scope, MemoryScope::Workspace | MemoryScope::Swarm) {
        return Ok(());
    }
    connection
        .visit_search_admitted_evidence_spans_in_current_snapshot(workspace_id, |span| {
            let Ok(id) = EvidenceId::from_str(&span.id) else {
                return Ok(());
            };
            if span.workspace_id != workspace_id
                || span.excerpt.trim().is_empty()
                || span.excerpt == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
                || !public_text(&span.excerpt)
            {
                return Ok(());
            }
            let Some(session) = connection.get_session(&span.session_id)? else {
                return Ok(());
            };
            if !span.is_direct_pack_admitted_for_session(workspace_id, &session) {
                return Ok(());
            }
            let Some(provenance_uri) = public_provenance(&span.canonical_provenance_uri()) else {
                return Ok(());
            };
            let mut source_memory_ids = Vec::new();
            if let Some(memory_id) = &span.memory_id {
                let Ok(memory_id) = MemoryId::from_str(memory_id) else {
                    return Ok(());
                };
                let Some(memory) = connection.get_memory(&memory_id.to_string())? else {
                    return Ok(());
                };
                if memory.workspace_id != workspace_id {
                    return Ok(());
                }
                // Lineage is only for correlated-support accounting. The
                // memory's body, lifecycle, confidence and trust are not used.
                source_memory_ids.push(memory_id.to_string());
            }
            let source = super::super::AskNativeSource {
                entity: crate::pack::PackEntityRef::EvidenceSpan(id),
                entity_revision: span.pack_entity_revision(),
                source_memory_ids,
            };
            let candidate = AskCandidate {
                memory_id: span.id.clone(),
                content: span.excerpt.clone(),
                // Imported excerpts have no calibrated memory confidence.
                // Use a neutral prior without claiming human verification.
                confidence: 0.5,
                trust_class: TrustClass::CassEvidence.as_str().to_owned(),
                provenance_uri: Some(provenance_uri),
                level: "episodic".to_owned(),
                kind: "evidence".to_owned(),
                team_provenance: None,
            };
            native_sources.insert(candidate.memory_id.clone(), source);
            candidates.push(candidate);
            Ok(())
        })
        .map_err(|_| super::corpus_storage_error())?;
    Ok(())
}

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

// The replay detector's bare-path boundary deliberately skips URI slashes.
// Use the shared path predicate too: file:///home/... must not become public
// merely because the slash is preceded by another slash instead of whitespace.
fn public_text(value: &str) -> bool {
    !redact_public_replay_text(value).redacted
        && !value
            .char_indices()
            .any(|(index, _)| crate::util::sensitive_path_starts_at(value, index))
}

fn public_label(value: &str) -> String {
    if public_text(value) {
        value.to_owned()
    } else {
        "[REDACTED]".to_owned()
    }
}

fn public_provenance(value: &str) -> Option<String> {
    if !public_text(value) {
        return None;
    }
    let uri = ProvenanceUri::from_str(value).ok()?;
    if let ProvenanceUri::File { path, .. } = &uri {
        // Check the parsed target, not the URI as a whole. This also covers
        // absolute roots outside the shared sensitive-prefix inventory and
        // Windows paths when the CLI is running on Unix.
        let drive_path = path.as_bytes().get(1) == Some(&b':')
            && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
        if path.starts_with(['/', '\\', '~'])
            || drive_path
            || path.split(['/', '\\']).any(|part| part == "..")
            || !public_text(path)
        {
            return None;
        }
    }
    let canonical = uri.to_string();
    public_text(&canonical).then_some(canonical)
}

pub(super) fn into_candidate(memory: StoredMemory) -> Option<AskCandidate> {
    let id = MemoryId::from_str(&memory.id).ok()?;
    let level = MemoryLevel::from_str(&memory.level).ok()?;
    let kind = MemoryKind::from_str(&memory.kind).ok()?;
    let trust = TrustClass::from_str(&memory.trust_class).ok()?;
    if memory.tombstoned_at.is_some()
        || memory.content.trim().is_empty()
        || memory.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
        || !public_text(&memory.content)
        // Custom kinds are supported. Check their raw spelling before the
        // kind parser normalizes case and separators in credential prefixes.
        || !public_text(&memory.kind)
        || !public_text(kind.as_str())
    {
        return None;
    }

    let provenance_uri = memory
        .provenance_uri
        .as_deref()
        .and_then(public_provenance)
        .unwrap_or_else(|| ProvenanceUri::EeMemory(id).to_string());
    let mut team_provenance = team_provenance_from_memory(&memory);
    if let Some(team) = &mut team_provenance {
        team.member_display_name = public_label(&team.member_display_name);
        team.project_name = team.project_name.as_deref().map(public_label);
        team.produced_at = public_label(&team.produced_at);
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
