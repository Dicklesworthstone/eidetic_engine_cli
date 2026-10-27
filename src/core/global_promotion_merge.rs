//! Reviewable global merges. Approximate similarity never authorizes a write
//! to a memory: it only creates a pending proposal in the destination store.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlmodel_core::Value as DbValue;

use super::{DEFAULT_PROMOTION_MERGE_SIMILARITY, PromotionPayload};
use crate::db::{
    CreateAuditInput, CreateCurationCandidateInput, DbConnection, StoredCurationCandidate,
    StoredMemory, generate_audit_id,
};
use crate::search::HashEmbedder;
use crate::search::simhash::{cosine_similarity, ranked_simhash_candidates, simhash_128};

const SOURCE_PREFIX: &str = "global-merge:v1:";
const SNAPSHOT_SCHEMA: &str = "ee.global_promotion.merge_snapshot.v1";
// Match remember --reinforce's bounded, deterministic similarity gate.
const SCAN_LIMIT: usize = 256;
const CANDIDATE_LIMIT: usize = 16;
const HAMMING_K: u32 = 32;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MergeSnapshot {
    schema: String,
    origin_workspace_id: String,
    origin_memory_id: String,
    origin_content_hash: String,
    origin_payload: Value,
    global_workspace_id: String,
    global_memory_id: String,
    target_snapshot_hash: String,
    target_authority_hash: String,
    proposed_content_hash: String,
}

pub(super) struct MergeProposal {
    pub(super) candidate_id: String,
    pub(super) global_memory_id: String,
    pub(super) already_applied: bool,
    source_id: String,
    content: String,
    reason: String,
    similarity: f32,
}

fn hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn invalid_snapshot() -> crate::db::DbError {
    super::global_mutation_error("Could not verify global merge authority snapshot")
}

fn candidate_id(source_id: &str) -> String {
    let digest = blake3::hash(source_id.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    let id = crate::models::CandidateId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string();
    format!("curate_{}", id.trim_start_matches("cand_"))
}

/// Bind every mutable authority field as well as the body. A review must not
/// overwrite a newer target, extend its lifetime, or cross a changed trust,
/// type, revision, structured-payload, or seal boundary.
fn target_snapshot(
    db: &DbConnection,
    memory: &StoredMemory,
    include_body: bool,
) -> crate::db::Result<String> {
    let rows = db.query(
        "SELECT superseded_at FROM memories WHERE workspace_id = ?1 AND id = ?2",
        &[
            DbValue::Text(memory.workspace_id.clone()),
            DbValue::Text(memory.id.clone()),
        ],
    )?;
    let superseded = match rows
        .first()
        .filter(|_| rows.len() == 1)
        .and_then(|row| row.get(0))
    {
        Some(DbValue::Null) => None,
        Some(DbValue::Text(value)) => Some(value.as_str()),
        _ => return Err(invalid_snapshot()),
    };
    let seal = db.get_memory_seal(&memory.id)?;
    let typed_fields = db.get_memory_typed_fields_json(&memory.id)?;
    let mut tags = db.get_memory_tags(&memory.id)?;
    tags.sort_unstable();
    tags.dedup();
    let mut snapshot = json!({
        "id": memory.id,
        "workspaceId": memory.workspace_id,
        "content": memory.content,
        "level": memory.level,
        "kind": memory.kind,
        "workflowId": memory.workflow_id,
        "confidence": memory.confidence,
        "utility": memory.utility,
        "importance": memory.importance,
        "trustClass": memory.trust_class,
        "trustSubclass": memory.trust_subclass,
        "provenanceUri": memory.provenance_uri,
        "provenanceChainHash": memory.provenance_chain_hash,
        "provenanceChainHashVersion": memory.provenance_chain_hash_version,
        "provenanceVerificationStatus": memory.provenance_verification_status,
        "provenanceVerifiedAt": memory.provenance_verified_at,
        "provenanceVerificationNote": memory.provenance_verification_note,
        "createdAt": memory.created_at,
        "updatedAt": memory.updated_at,
        "tombstonedAt": memory.tombstoned_at,
        "validFrom": memory.valid_from,
        "validTo": memory.valid_to,
        "supersededAt": superseded,
        "seal": seal,
        "typedFields": typed_fields,
        "tags": tags,
    });
    if !include_body {
        let object = snapshot.as_object_mut().ok_or_else(invalid_snapshot)?;
        object.remove("content");
        object.remove("updatedAt");
        // Subsequent outcome evidence can adjust scores without erasing the
        // accepted origin's contribution. Apply still binds these fields.
        object.remove("confidence");
        object.remove("utility");
        object.remove("importance");
        // Curation records new provenance for the deliberately changed body.
        object.remove("provenanceChainHash");
        object.remove("provenanceChainHashVersion");
        object.remove("provenanceVerificationStatus");
        object.remove("provenanceVerifiedAt");
        object.remove("provenanceVerificationNote");
    }
    Ok(hash(snapshot.to_string().as_bytes()))
}

fn applied_proposal(
    db: &DbConnection,
    workspace: &str,
    source: &StoredMemory,
    payload: &PromotionPayload,
    reference: DateTime<Utc>,
) -> crate::db::Result<Option<MergeProposal>> {
    // IDs and hashes are canonical ASCII. A bound LIKE parameter performs a
    // narrow provenance lookup without reading unrelated candidate bodies.
    let pattern = format!(
        "{SOURCE_PREFIX}%\"originWorkspaceId\":\"{}\",\"originMemoryId\":\"{}\",\"originContentHash\":\"{}\"%",
        source.workspace_id,
        source.id,
        hash(source.content.as_bytes())
    );
    let rows = db.query(
        "SELECT id FROM curation_candidates WHERE workspace_id = ?1 AND candidate_type = 'merge' AND source_type = 'rule_engine' AND status = 'applied' AND source_id LIKE ?2 ORDER BY applied_at DESC, id ASC LIMIT ?3",
        &[DbValue::Text(workspace.to_owned()), DbValue::Text(pattern), DbValue::BigInt(SCAN_LIMIT as i64)],
    )?;
    for row in rows {
        let Some(DbValue::Text(id)) = row.get(0) else {
            return Err(invalid_snapshot());
        };
        let candidate = db
            .get_curation_candidate(workspace, id)?
            .ok_or_else(invalid_snapshot)?;
        let Some(source_id) = candidate.source_id.as_deref() else {
            continue;
        };
        let Some(raw) = source_id.strip_prefix(SOURCE_PREFIX) else {
            continue;
        };
        let snapshot: MergeSnapshot = serde_json::from_str(raw).map_err(|_| invalid_snapshot())?;
        if snapshot.origin_workspace_id != source.workspace_id
            || snapshot.origin_memory_id != source.id
            || snapshot.origin_content_hash != hash(source.content.as_bytes())
            || snapshot.origin_payload != payload.audit_evidence()
            || snapshot.global_workspace_id != workspace
        {
            continue;
        }
        validate_merge_candidate(db, &candidate).map_err(|_| invalid_snapshot())?;
        if !crate::core::memory_lifecycle::pending_memory_review_ids(
            db,
            workspace,
            &[snapshot.global_memory_id.as_str()],
        )?
        .is_empty()
        {
            continue;
        }
        let Some(target) = db.get_memory(&snapshot.global_memory_id)? else {
            continue;
        };
        if target.workspace_id != workspace
            || !super::backflow_target_is_current(db, &target, reference)?
            || hash(target.content.as_bytes()) != snapshot.proposed_content_hash
            || target_snapshot(db, &target, false)? != snapshot.target_authority_hash
        {
            continue;
        }
        return Ok(Some(MergeProposal {
            candidate_id: candidate.id,
            global_memory_id: target.id,
            already_applied: true,
            source_id: source_id.to_owned(),
            content: candidate.proposed_content.ok_or_else(invalid_snapshot)?,
            reason: candidate.reason,
            similarity: candidate.confidence,
        }));
    }
    Ok(None)
}

fn instant(value: Option<&str>) -> crate::db::Result<Option<DateTime<Utc>>> {
    value
        .map(|raw| {
            DateTime::parse_from_rfc3339(raw)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|_| invalid_snapshot())
        })
        .transpose()
}

/// The caller owns a coherent read snapshot or writer transaction and has
/// already checked exact twins. Read at most one bounded window; use the
/// existing SimHash and cosine primitives, with deterministic tie breaking.
pub(super) fn find_proposal(
    db: &DbConnection,
    workspace: &str,
    source: &StoredMemory,
    payload: &PromotionPayload,
    reference: DateTime<Utc>,
) -> crate::db::Result<Option<MergeProposal>> {
    if let Some(applied) = applied_proposal(db, workspace, source, payload, reference)? {
        return Ok(Some(applied));
    }
    let rows = db.query(
        "SELECT id FROM memories WHERE workspace_id = ?1 AND content != ?2 AND level = ?3 AND kind = ?4 AND trust_class = ?5 AND trust_subclass IS ?6 AND tombstoned_at IS NULL ORDER BY id DESC LIMIT ?7",
        &[
            DbValue::Text(workspace.to_owned()),
            DbValue::Text(source.content.clone()),
            DbValue::Text(source.level.clone()),
            DbValue::Text(source.kind.clone()),
            DbValue::Text(source.trust_class.clone()),
            source.trust_subclass.clone().map_or(DbValue::Null, DbValue::Text),
            DbValue::BigInt(SCAN_LIMIT as i64),
        ],
    )?;
    let ids = rows
        .iter()
        .map(|row| {
            row.get(0)
                .and_then(DbValue::as_str)
                .ok_or_else(invalid_snapshot)
        })
        .collect::<crate::db::Result<Vec<_>>>()?;
    let held = crate::core::memory_lifecycle::pending_memory_review_ids(db, workspace, &ids)?;
    let mut memories = Vec::new();
    for id in ids {
        if held.contains(id) {
            continue;
        }
        let memory = db.get_memory(id)?.ok_or_else(invalid_snapshot)?;
        if !super::backflow_target_is_current(db, &memory, reference)?
            || instant(memory.valid_to.as_deref())? != instant(source.valid_to.as_deref())?
        {
            continue;
        }
        if instant(memory.valid_from.as_deref())? != instant(source.valid_from.as_deref())? {
            let Some(proof) = payload.default_start() else {
                continue;
            };
            if !super::validity::independent_default_starts(db, source, proof, &memory)? {
                continue;
            }
        }
        let fields = db.get_memory_typed_fields_json(id)?;
        if !payload.matches_fields(&source.kind, fields.as_deref())?
            || crate::policy::redact_secret_like_content(&memory.content).redacted
            || memory
                .content
                .len()
                .saturating_add(source.content.len())
                .saturating_add(2)
                > crate::models::MAX_CONTENT_BYTES
        {
            continue;
        }
        memories.push(memory);
    }
    let gated = ranked_simhash_candidates(
        simhash_128(&source.content),
        memories
            .iter()
            .map(|memory| (memory.id.as_str(), simhash_128(&memory.content))),
        HAMMING_K,
        CANDIDATE_LIMIT,
    );
    let embedder = HashEmbedder::default_256();
    let embedding = embedder.embed_sync(&source.content);
    let mut best: Option<(&StoredMemory, f32, u32)> = None;
    for candidate in gated {
        let Some(memory) = memories
            .iter()
            .find(|memory| memory.id == candidate.candidate_id)
        else {
            return Err(invalid_snapshot());
        };
        let Some(similarity) = cosine_similarity(&embedding, &embedder.embed_sync(&memory.content))
        else {
            continue;
        };
        if similarity < DEFAULT_PROMOTION_MERGE_SIMILARITY {
            continue;
        }
        if best.as_ref().is_none_or(|(current, score, distance)| {
            similarity > *score
                || (similarity == *score
                    && (candidate.hamming_distance, memory.id.as_str())
                        < (*distance, current.id.as_str()))
        }) {
            best = Some((memory, similarity, candidate.hamming_distance));
        }
    }
    let Some((target, similarity, _)) = best else {
        return Ok(None);
    };
    // Preserve both versions for deliberate review. No model-generated
    // synthesis or confidence/trust promotion is authorized by similarity.
    let content = format!("{}\n\n{}", target.content, source.content);
    let snapshot = MergeSnapshot {
        schema: SNAPSHOT_SCHEMA.to_owned(),
        origin_workspace_id: source.workspace_id.clone(),
        origin_memory_id: source.id.clone(),
        origin_content_hash: hash(source.content.as_bytes()),
        origin_payload: payload.audit_evidence(),
        global_workspace_id: workspace.to_owned(),
        global_memory_id: target.id.clone(),
        target_snapshot_hash: target_snapshot(db, target, true)?,
        target_authority_hash: target_snapshot(db, target, false)?,
        proposed_content_hash: hash(content.as_bytes()),
    };
    let source_id = format!(
        "{SOURCE_PREFIX}{}",
        serde_json::to_string(&snapshot).map_err(|_| invalid_snapshot())?
    );
    Ok(Some(MergeProposal {
        candidate_id: candidate_id(&source_id),
        global_memory_id: target.id.clone(),
        already_applied: false,
        source_id,
        content,
        reason: format!(
            "Review near-duplicate global knowledge from {} against global memory {}. The proposed text preserves both versions; accepting and applying this candidate updates only the existing global memory.",
            super::promotion_provenance_uri(&source.workspace_id, &source.id),
            target.id
        ),
        similarity,
    }))
}

/// Persist candidate and destination provenance together. Retries return the
/// same row, including a reviewer's terminal decision, instead of requeueing.
pub(super) fn persist_proposal(
    db: &DbConnection,
    workspace: &str,
    proposal: &MergeProposal,
    actor: Option<&str>,
) -> crate::db::Result<()> {
    if let Some(existing) = db.get_curation_candidate(workspace, &proposal.candidate_id)? {
        if existing.source_id.as_deref() != Some(proposal.source_id.as_str())
            || existing.proposed_content.as_deref() != Some(proposal.content.as_str())
            || existing.target_memory_id.as_deref() != Some(proposal.global_memory_id.as_str())
            || existing.candidate_type != "merge"
            || existing.source_type != "rule_engine"
        {
            return Err(invalid_snapshot());
        }
        return Ok(());
    }
    db.insert_curation_candidate(
        &proposal.candidate_id,
        &CreateCurationCandidateInput {
            workspace_id: workspace.to_owned(),
            candidate_type: "merge".to_owned(),
            target_memory_id: Some(proposal.global_memory_id.clone()),
            proposed_content: Some(proposal.content.clone()),
            proposed_confidence: None,
            proposed_trust_class: None,
            source_type: "rule_engine".to_owned(),
            source_id: Some(proposal.source_id.clone()),
            reason: proposal.reason.clone(),
            confidence: proposal.similarity,
            status: Some("pending".to_owned()),
            created_at: None,
            ttl_expires_at: None,
            derivation_source_refs_json: None,
            derivation_metadata_json: None,
        },
    )?;
    db.insert_audit(
        &generate_audit_id(),
        &CreateAuditInput {
            workspace_id: Some(workspace.to_owned()),
            actor: actor.map(str::to_owned),
            action: "memory.promote_global_proposed".to_owned(),
            target_type: Some("curation_candidate".to_owned()),
            target_id: Some(proposal.candidate_id.clone()),
            details: Some(
                json!({
                    "schema": SNAPSHOT_SCHEMA,
                    "globalMemoryId": proposal.global_memory_id,
                    "mergeCandidateId": proposal.candidate_id,
                    "sourceId": proposal.source_id,
                })
                .to_string(),
            ),
        },
    )?;
    Ok(())
}

/// Called by curation's read reports and again under its write transaction.
/// Ordinary candidates retain their existing validation behavior.
pub(crate) fn validate_merge_candidate(
    db: &DbConnection,
    candidate: &StoredCurationCandidate,
) -> Result<(), String> {
    let Some(source_id) = candidate.source_id.as_deref() else {
        return Ok(());
    };
    if !source_id.starts_with("global-merge:") {
        return Ok(());
    }
    let invalid = || {
        "Global merge proposal or target authority changed; generate a fresh promotion proposal before applying.".to_owned()
    };
    let raw = source_id.strip_prefix(SOURCE_PREFIX).ok_or_else(invalid)?;
    let snapshot: MergeSnapshot = serde_json::from_str(raw).map_err(|_| invalid())?;
    let content = candidate.proposed_content.as_deref().ok_or_else(invalid)?;
    if snapshot.schema != SNAPSHOT_SCHEMA
        || candidate.id != candidate_id(source_id)
        || candidate.candidate_type != "merge"
        || candidate.source_type != "rule_engine"
        || candidate.workspace_id != snapshot.global_workspace_id
        || candidate.target_memory_id.as_deref() != Some(snapshot.global_memory_id.as_str())
        || candidate.proposed_confidence.is_some()
        || candidate.proposed_trust_class.is_some()
        || candidate.derivation_source_refs_json.is_some()
        || candidate.derivation_metadata_json.is_some()
        || hash(content.as_bytes()) != snapshot.proposed_content_hash
    {
        return Err(invalid());
    }
    let current = db
        .get_curation_candidate(&candidate.workspace_id, &candidate.id)
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    if &current != candidate {
        return Err(invalid());
    }
    // Applied candidates are immutable historical receipts. Generic curation
    // already prevents applying them twice; later target edits must not turn
    // a completed review into a purportedly pending stale write.
    if candidate.status == "applied" && candidate.applied_at.is_some() {
        return Ok(());
    }
    if !crate::core::memory_lifecycle::pending_memory_review_ids(
        db,
        &snapshot.global_workspace_id,
        &[snapshot.global_memory_id.as_str()],
    )
    .map_err(|_| invalid())?
    .is_empty()
    {
        return Err(invalid());
    }
    let target = db
        .get_memory(&snapshot.global_memory_id)
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    if target.workspace_id != snapshot.global_workspace_id
        || !super::backflow_target_is_current(db, &target, Utc::now()).map_err(|_| invalid())?
        || target_snapshot(db, &target, true).map_err(|_| invalid())?
            != snapshot.target_snapshot_hash
    {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn review_command(
    paths: &super::super::global_store::GlobalStorePaths,
    id: &str,
) -> String {
    let quote =
        |path: &std::path::Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    format!(
        "ee curate show {id} --workspace {} --database {} --json",
        quote(&paths.root),
        quote(&paths.database_path)
    )
}

/// The reviewed candidate owns one durable publication receipt. Reapplying an
/// already-applied candidate recovers this receipt instead of rewriting text.
pub(crate) fn merge_index_job_id(candidate: &StoredCurationCandidate) -> Option<String> {
    candidate
        .source_id
        .as_deref()?
        .strip_prefix(SOURCE_PREFIX)?;
    let suffix = candidate.id.strip_prefix("curate_")?;
    Some(format!("sidx_{suffix}"))
}

/// Resolve global publication before applying a mutation. An explicit custom
/// database for an ordinary curation candidate retains its existing behavior.
pub(crate) fn merge_index_paths(
    db: &DbConnection,
    candidate: &StoredCurationCandidate,
    workspace_path: &std::path::Path,
    database_path: &std::path::Path,
) -> Result<Option<super::super::global_store::GlobalStorePaths>, String> {
    if merge_index_job_id(candidate).is_none() {
        return Ok(None);
    }
    let paths = super::super::global_store::GlobalStorePaths::from_root(workspace_path);
    let invalid = || {
        "Global merge application requires the bound global workspace and its ee.db; use the promotion report's explicit review routing.".to_owned()
    };
    if std::fs::canonicalize(database_path).map_err(|_| invalid())?
        != std::fs::canonicalize(&paths.database_path).map_err(|_| invalid())?
        || super::admission::bound_global_workspace(db, &paths)? != candidate.workspace_id
    {
        return Err(invalid());
    }
    Ok(Some(paths))
}

/// Called after the memory update while the curation writer still owns the
/// transaction. A failed obligation rolls back the memory and review together.
pub(crate) fn persist_merge_index_job(
    db: &DbConnection,
    candidate: &StoredCurationCandidate,
) -> crate::db::Result<()> {
    let Some(id) = merge_index_job_id(candidate) else {
        return Ok(());
    };
    let target = candidate
        .target_memory_id
        .as_ref()
        .ok_or_else(invalid_snapshot)?;
    db.insert_search_index_job(
        &id,
        &crate::db::CreateSearchIndexJobInput {
            workspace_id: candidate.workspace_id.clone(),
            job_type: crate::db::SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(target.clone()),
            documents_total: 1,
        },
    )
}

#[cfg(test)]
#[path = "global_promotion_merge_tests.rs"]
mod tests;
