//! Explicit conflict plans carried through the existing curation review queue.
//!
//! The receipt binds the request to both complete source snapshots. Review is
//! not permission to apply a different plan: the decision writer checks the
//! receipt again under its writer fence and commits candidate completion with
//! the rationale, source effects, lineage, and terminal conflict audit.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::contradiction_detect::{
    CONFLICT_MERGE_MAX_BYTES, ConflictResolutionOutcome, ConflictResolutionPlan,
    ConflictResolveRequest, ContradictionDetectionConfig, PlannedResolutionAction, ResolveVerb,
    assemble_conflict_surface_for_workspace, merge_source_is_supported, plan_conflict_resolution,
};
use crate::db::{
    CreateAuditInput, CreateCurationCandidateInput, DbConnection, StoredCurationCandidate,
    StoredMemory, audit_actions, generate_audit_id,
};
use crate::models::{CandidateId, DomainError, MemoryId, Tag};
use sqlmodel_core::Value as DbValue;

pub const REPORT_SCHEMA: &str = "ee.conflict.proposal.v1";
const RECEIPT_SCHEMA: &str = "ee.conflict.proposal_receipt.v1";
const SOURCE_PREFIX: &str = "conflict-resolution:v1:";
pub(crate) const STALE_CODE: &str = "conflict_proposal_stale";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProposalReceipt {
    schema: String,
    workspace_id: String,
    memory_a: String,
    memory_b: String,
    verb: String,
    keep: Option<String>,
    reason: String,
    scope_a_tags: Vec<String>,
    scope_b_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    merge_content: Option<String>,
    source_a_hash: String,
    source_b_hash: String,
    plan_hash: String,
    /// Retain the exact reviewed effects for existing curate show payloads,
    /// even after a source changes or the resolution has been applied.
    plan: serde_json::Value,
}

#[derive(Debug)]
pub(crate) struct CheckedProposal {
    pub plan: ConflictResolutionPlan,
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictProposalReport {
    pub schema: &'static str,
    pub status: &'static str,
    pub candidate_id: String,
    pub candidate_status: String,
    pub dry_run: bool,
    pub persisted: bool,
    pub created: bool,
    pub plan: ConflictResolutionPlan,
    pub next_commands: Vec<String>,
}

/// Recognize the reserved producer before generic contradiction-review link
/// dispatch. A changed producer label must not reinterpret a typed receipt.
pub(crate) fn is_candidate(candidate: &StoredCurationCandidate) -> bool {
    candidate
        .source_id
        .as_deref()
        .is_some_and(|id| id.starts_with("conflict-resolution:"))
        || candidate
            .proposed_content
            .as_deref()
            .is_some_and(|content| {
                serde_json::from_str::<serde_json::Value>(content)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("schema")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned)
                    })
                    .is_some_and(|schema| schema.starts_with("ee.conflict.proposal_receipt."))
            })
}

fn refusal(code: &'static str, message: impl Into<String>) -> DomainError {
    DomainError::UsageCodeWithDetails {
        code,
        message: message.into(),
        repair: Some("Inspect ee conflict list and create a fresh --propose request.".to_owned()),
        details_json: "{}".to_owned(),
    }
}

fn storage_error(_: impl std::fmt::Display) -> DomainError {
    DomainError::Storage {
        message: "Could not verify or persist the complete conflict proposal.".to_owned(),
        repair: Some("ee doctor --json".to_owned()),
    }
}

fn digest(value: &impl Serialize) -> Result<String, DomainError> {
    serde_json::to_vec(value)
        .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
        .map_err(storage_error)
}

fn candidate_id(receipt_hash: &str) -> String {
    let hash = blake3::hash(format!("{SOURCE_PREFIX}{receipt_hash}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    let id = CandidateId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string();
    format!("curate_{}", id.trim_start_matches("cand_"))
}

fn source_hash(
    connection: &DbConnection,
    workspace_id: &str,
    id: &str,
) -> Result<String, DomainError> {
    let seals =
        crate::core::memory_lifecycle::load_memory_seals_for_admission(connection, workspace_id)
            .map_err(storage_error)?;
    let source_seals: Vec<_> = seals.iter().filter(|seal| seal.memory_id == id).collect();
    let held =
        crate::core::memory_lifecycle::pending_memory_review_ids(connection, workspace_id, &[id])
            .map_err(storage_error)?;
    if source_seals.iter().any(|seal| seal.is_sealed()) || !held.is_empty() {
        return Err(refusal(
            STALE_CODE,
            "A conflict source is sealed or awaiting memory review.",
        ));
    }
    let memory = connection
        .get_memory(id)
        .map_err(storage_error)?
        .ok_or_else(|| refusal(STALE_CODE, "A conflict source no longer exists."))?;
    if memory.workspace_id != workspace_id
        || memory.tombstoned_at.is_some()
        || memory.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
    {
        return Err(refusal(
            STALE_CODE,
            "A conflict source no longer has live workspace authority.",
        ));
    }
    let mut tags = connection.get_memory_tags(id).map_err(storage_error)?;
    tags.sort();
    let typed_fields = connection
        .get_memory_typed_fields_json(id)
        .map_err(storage_error)?;
    let superseded_at = connection
        .get_memory_superseded_at(id)
        .map_err(storage_error)?;
    let logical_id = connection
        .get_memory_logical_id(id)
        .map_err(storage_error)?;
    // Include authority, lifetime, source revision, and scope as well as the
    // body. A byte-identical body at a different trust level is not the plan
    // the reviewer accepted. Hashes keep private bodies out of queue payloads.
    digest(&json!({
        "id": memory.id, "workspaceId": memory.workspace_id,
        "level": memory.level, "kind": memory.kind, "content": memory.content,
        "workflowId": memory.workflow_id, "confidence": memory.confidence,
        "utility": memory.utility, "importance": memory.importance,
        "provenanceUri": memory.provenance_uri, "trustClass": memory.trust_class,
        "trustSubclass": memory.trust_subclass,
        "provenanceChainHash": memory.provenance_chain_hash,
        "provenanceChainHashVersion": memory.provenance_chain_hash_version,
        "provenanceVerificationStatus": memory.provenance_verification_status,
        "provenanceVerifiedAt": memory.provenance_verified_at,
        "provenanceVerificationNote": memory.provenance_verification_note,
        "createdAt": memory.created_at, "updatedAt": memory.updated_at,
        "tombstonedAt": memory.tombstoned_at,
        "validFrom": memory.valid_from, "validTo": memory.valid_to,
        "tags": tags, "typedFields": typed_fields, "supersededAt": superseded_at,
        "logicalId": logical_id,
        "seals": format!("{source_seals:?}"),
    }))
}

pub(crate) fn live_plan(
    connection: &DbConnection,
    workspace_id: &str,
    request: &ConflictResolveRequest<'_>,
) -> Result<ConflictResolutionPlan, DomainError> {
    let surface = assemble_conflict_surface_for_workspace(
        connection,
        workspace_id,
        ContradictionDetectionConfig::default(),
    );
    if !surface.degraded.is_empty() {
        return Err(refusal(
            STALE_CODE,
            "The live conflict surface could not be completely verified.",
        ));
    }
    match plan_conflict_resolution(&surface, request) {
        ConflictResolutionOutcome::Plan(plan) => {
            if plan.verb == ResolveVerb::Merge {
                merge_sources(connection, workspace_id, &plan)?;
            }
            Ok(plan)
        }
        ConflictResolutionOutcome::StaleSurface { .. } => Err(refusal(
            STALE_CODE,
            "This conflict was resolved or its source surface changed after proposal.",
        )),
        ConflictResolutionOutcome::PolicyDenied { message, repair } => {
            Err(DomainError::PolicyDenied {
                message,
                repair: Some(repair),
            })
        }
        ConflictResolutionOutcome::InvalidRequest { message, repair } => Err(DomainError::Usage {
            message,
            repair: Some(repair),
        }),
    }
}

/// The source facts and exact scope a reviewed textual merge may preserve.
/// Callers still bind the complete source snapshots and the live conflict;
/// this additional check prevents a summary from replacing typed behavior or
/// silently widening a workflow, tag scope, or validity window.
pub(crate) struct MergeSources {
    pub source_a: StoredMemory,
    pub source_b: StoredMemory,
    pub tags: Vec<String>,
    pub valid_to: Option<String>,
}

pub(crate) fn merge_sources(
    connection: &DbConnection,
    workspace_id: &str,
    plan: &ConflictResolutionPlan,
) -> Result<MergeSources, DomainError> {
    let incompatible = || {
        refusal(
            "conflict_merge_sources_incompatible",
            "Merge requires current textual facts or summaries with the same workflow and tags; typed or procedural sources need their dedicated resolution workflow.",
        )
    };
    let load = |id: &str| {
        // Source authority includes seals and outstanding human review, not
        // merely the visibility of a source body on an earlier conflict list.
        source_hash(connection, workspace_id, id)?;
        connection
            .get_memory(id)
            .map_err(storage_error)?
            .ok_or_else(incompatible)
    };
    let source_a = load(&plan.memory_a)?;
    let source_b = load(&plan.memory_b)?;
    let now = Utc::now();
    let mut earliest_end: Option<(DateTime<Utc>, String)> = None;
    for source in [&source_a, &source_b] {
        if source.workspace_id != workspace_id
            || source.tombstoned_at.is_some()
            || !merge_source_is_supported(&source.level, &source.kind)
            || connection
                .get_memory_superseded_at(&source.id)
                .map_err(storage_error)?
                .is_some()
            || [source.confidence, source.utility, source.importance]
                .into_iter()
                .any(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(incompatible());
        }
        if let Some(fields) = connection
            .get_memory_typed_fields_json(&source.id)
            .map_err(storage_error)?
            && !serde_json::from_str::<serde_json::Value>(&fields)
                .ok()
                .and_then(|value| value.as_object().map(|fields| fields.is_empty()))
                .unwrap_or(false)
        {
            return Err(incompatible());
        }
        if let Some(start) = source.valid_from.as_deref() {
            let start = DateTime::parse_from_rfc3339(start)
                .map_err(|_| incompatible())?
                .with_timezone(&Utc);
            if start > now {
                return Err(incompatible());
            }
        }
        if let Some(end) = source.valid_to.as_deref() {
            let parsed = DateTime::parse_from_rfc3339(end)
                .map_err(|_| incompatible())?
                .with_timezone(&Utc);
            if parsed <= now {
                return Err(incompatible());
            }
            if earliest_end
                .as_ref()
                .is_none_or(|(earliest, _)| parsed < *earliest)
            {
                earliest_end = Some((parsed, end.to_owned()));
            }
        }
    }
    let source_tags = |id: &str| {
        let tags = connection.get_memory_tags(id).map_err(storage_error)?;
        normalized_scope_tags(&tags)
    };
    let tags = source_tags(&source_a.id)?;
    if source_a.workflow_id != source_b.workflow_id || tags != source_tags(&source_b.id)? {
        return Err(incompatible());
    }
    Ok(MergeSources {
        source_a,
        source_b,
        tags,
        valid_to: earliest_end.map(|(_, original)| original),
    })
}

fn check_text(text: &str) -> Result<(), DomainError> {
    if text.trim().is_empty()
        || text.len() > 4096
        || crate::policy::redact_public_replay_text(text).redacted
    {
        return Err(refusal(
            "conflict_proposal_invalid",
            "Conflict rationale and scope labels must be bounded, non-secret text without embedded instructions.",
        ));
    }
    Ok(())
}

fn normalized_scope_tags(tags: &[String]) -> Result<Vec<String>, DomainError> {
    let mut normalized = Vec::with_capacity(tags.len());
    for raw in tags {
        check_text(raw)?;
        let tag = Tag::parse(raw).map_err(|_| {
            refusal(
                "conflict_proposal_invalid",
                "Each conflict scope label must be a valid memory tag of at most 64 UTF-8 bytes.",
            )
        })?;
        normalized.push(tag.as_str().to_owned());
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

/// Caller holds the proposal writer transaction. Only queue and audit rows are
/// inserted; memories, source tags, links, decisions, and index jobs are untouched.
pub(crate) fn create_in_transaction(
    connection: &DbConnection,
    workspace_id: &str,
    request: &ConflictResolveRequest<'_>,
    actor: Option<&str>,
) -> Result<ConflictProposalReport, DomainError> {
    if request.scope_a_tags.len() > 64 || request.scope_b_tags.len() > 64 {
        return Err(refusal(
            "conflict_proposal_invalid",
            "Each conflict scope accepts at most 64 tags.",
        ));
    }
    check_text(request.reason)?;
    // Scope effects must use canonical storage tags before the plan is hashed.
    // Otherwise INSERT OR IGNORE can silently drop an invalid label while the
    // resolution becomes terminal, leaving a source without its reviewed scope.
    let request = ConflictResolveRequest {
        scope_a_tags: normalized_scope_tags(&request.scope_a_tags)?,
        scope_b_tags: normalized_scope_tags(&request.scope_b_tags)?,
        ..(*request).clone()
    };
    let plan = live_plan(connection, workspace_id, &request)?;
    let tags_for = |id: &str| {
        plan.actions
            .iter()
            .find_map(|action| match action {
                PlannedResolutionAction::AddTags { memory_id, tags } if memory_id == id => {
                    Some(tags.clone())
                }
                _ => None,
            })
            .unwrap_or_default()
    };
    let receipt = ProposalReceipt {
        schema: RECEIPT_SCHEMA.to_owned(),
        workspace_id: workspace_id.to_owned(),
        memory_a: plan.memory_a.clone(),
        memory_b: plan.memory_b.clone(),
        verb: plan.verb.as_str().to_owned(),
        keep: plan.keep.clone(),
        reason: request.reason.to_owned(),
        scope_a_tags: tags_for(&plan.memory_a),
        scope_b_tags: tags_for(&plan.memory_b),
        merge_content: plan.actions.iter().find_map(|action| match action {
            PlannedResolutionAction::MergeMemories { content } => Some(content.clone()),
            _ => None,
        }),
        source_a_hash: source_hash(connection, workspace_id, &plan.memory_a)?,
        source_b_hash: source_hash(connection, workspace_id, &plan.memory_b)?,
        plan_hash: digest(&plan)?,
        plan: serde_json::to_value(&plan).map_err(storage_error)?,
    };
    let receipt_hash = digest(&receipt)?;
    let id = candidate_id(&receipt_hash);
    let content = serde_json::to_string(&receipt).map_err(storage_error)?;
    if content.len() > 65536 {
        return Err(refusal(
            "conflict_proposal_invalid",
            "The complete conflict proposal must fit within 64 KiB.",
        ));
    }
    let source_id = format!("{SOURCE_PREFIX}{receipt_hash}");
    let existing = connection
        .get_curation_candidate(workspace_id, &id)
        .map_err(storage_error)?;
    let created = existing.is_none();
    let candidate_status = if let Some(existing) = existing {
        if existing.proposed_content.as_deref() != Some(content.as_str())
            || existing.source_id.as_deref() != Some(source_id.as_str())
            || existing.candidate_type != "contradiction_review"
            || existing.source_type != "contradiction_detected"
            || existing.target_memory_id.as_deref() != Some(plan.memory_a.as_str())
        {
            return Err(refusal(
                "conflict_proposal_invalid",
                "The existing proposal no longer matches its immutable receipt.",
            ));
        }
        validate_receipt(&existing)?;
        existing.status
    } else {
        connection
            .insert_curation_candidate(
                &id,
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.to_owned(),
                    candidate_type: "contradiction_review".to_owned(),
                    target_memory_id: Some(plan.memory_a.clone()),
                    proposed_content: Some(content),
                    proposed_confidence: None,
                    proposed_trust_class: None,
                    source_type: "contradiction_detected".to_owned(),
                    source_id: Some(source_id),
                    reason: request.reason.to_owned(),
                    confidence: 0.5,
                    status: Some("pending".to_owned()),
                    created_at: None,
                    ttl_expires_at: None,
                    derivation_source_refs_json: None,
                    derivation_metadata_json: None,
                },
            )
            .map_err(storage_error)?;
        connection.insert_audit(&generate_audit_id(), &CreateAuditInput {
            workspace_id: Some(workspace_id.to_owned()), actor: actor.map(str::to_owned),
            action: audit_actions::CURATION_CANDIDATE_CREATE.to_owned(),
            target_type: Some("curation_candidate".to_owned()), target_id: Some(id.clone()),
            details: Some(json!({"schema": REPORT_SCHEMA, "candidateId": id, "conflictId": plan.conflict_id, "verb": receipt.verb, "receiptHash": receipt_hash, "status": "pending"}).to_string()),
        }).map_err(storage_error)?;
        "pending".to_owned()
    };
    Ok(ConflictProposalReport {
        schema: REPORT_SCHEMA,
        status: "proposed",
        candidate_id: id,
        candidate_status,
        dry_run: false,
        persisted: true,
        created,
        plan,
        next_commands: Vec::new(),
    })
}

fn decode_receipt(candidate: &StoredCurationCandidate) -> Result<ProposalReceipt, DomainError> {
    let invalid = || {
        refusal(
            "conflict_proposal_invalid",
            "Conflict proposal payload, producer, or immutable receipt is invalid.",
        )
    };
    if candidate.candidate_type != "contradiction_review"
        || candidate.source_type != "contradiction_detected"
        || candidate.proposed_confidence.is_some()
        || candidate.proposed_trust_class.is_some()
        || candidate.derivation_source_refs_json.is_some()
        || candidate.derivation_metadata_json.is_some()
        || candidate.merged_into_candidate_id.is_some()
        || candidate.confidence != 0.5
    {
        return Err(invalid());
    }
    let content = candidate.proposed_content.as_deref().ok_or_else(invalid)?;
    if content.len() > 65536 {
        return Err(invalid());
    }
    let receipt: ProposalReceipt = serde_json::from_str(content).map_err(|_| invalid())?;
    let receipt_hash = digest(&receipt)?;
    if receipt.schema != RECEIPT_SCHEMA
        || receipt.workspace_id != candidate.workspace_id
        || candidate.id != candidate_id(&receipt_hash)
        || candidate.source_id.as_deref() != Some(format!("{SOURCE_PREFIX}{receipt_hash}").as_str())
        || candidate.target_memory_id.as_deref() != Some(receipt.memory_a.as_str())
        || candidate.reason != receipt.reason
        || receipt.scope_a_tags.len() > 64
        || receipt.scope_b_tags.len() > 64
    {
        return Err(invalid());
    }
    check_text(&receipt.reason)?;
    if normalized_scope_tags(&receipt.scope_a_tags)? != receipt.scope_a_tags
        || normalized_scope_tags(&receipt.scope_b_tags)? != receipt.scope_b_tags
    {
        return Err(invalid());
    }
    match (receipt.verb.as_str(), receipt.merge_content.as_deref()) {
        ("merge", Some(content))
            if !content.is_empty()
                && content == content.trim()
                && content.len() <= CONFLICT_MERGE_MAX_BYTES
                && !crate::policy::redact_public_replay_body(content).redacted => {}
        ("merge", _) | (_, Some(_)) => return Err(invalid()),
        (_, None) => {}
    }
    Ok(receipt)
}

pub(crate) fn validate_receipt(candidate: &StoredCurationCandidate) -> Result<(), DomainError> {
    decode_receipt(candidate).map(|_| ())
}

/// Human inspection uses the existing proposed-content field, without adding
/// a new public show schema field or displaying source snapshot hashes.
pub(crate) fn public_reviewed_plan_text(content: &str) -> Option<String> {
    if content.len() > 65536 {
        return None;
    }
    let receipt: ProposalReceipt = serde_json::from_str(content).ok()?;
    if receipt.schema != RECEIPT_SCHEMA {
        return None;
    }
    let text = serde_json::to_string_pretty(&receipt.plan).ok()?;
    let screened = crate::policy::redact_public_replay_body(&text);
    (!screened.redacted).then_some(screened.content)
}

pub(crate) fn validate_candidate(
    connection: &DbConnection,
    candidate: &StoredCurationCandidate,
    require_approved: bool,
) -> Result<CheckedProposal, DomainError> {
    let receipt = decode_receipt(candidate)?;
    if require_approved && (candidate.status != "approved" || candidate.review_state != "accepted")
    {
        return Err(refusal(
            "candidate_not_approved",
            "Approve this conflict proposal with ee curate accept or validate before applying it.",
        ));
    }
    if !matches!(candidate.status.as_str(), "pending" | "approved") {
        return Err(refusal(
            "candidate_status_terminal",
            "This proposal is no longer available for review or application.",
        ));
    }
    if let Some(expires) = &candidate.ttl_expires_at {
        let expires = DateTime::parse_from_rfc3339(expires).map_err(|_| {
            refusal(
                "conflict_proposal_invalid",
                "Conflict proposal TTL is not a valid timestamp.",
            )
        })?;
        if expires <= Utc::now() {
            return Err(refusal(
                "candidate_expired",
                "The conflict proposal TTL has expired.",
            ));
        }
    }
    if source_hash(connection, &receipt.workspace_id, &receipt.memory_a)? != receipt.source_a_hash
        || source_hash(connection, &receipt.workspace_id, &receipt.memory_b)?
            != receipt.source_b_hash
    {
        return Err(refusal(
            STALE_CODE,
            "Conflict source content, revision, scope, lifetime, or authority changed after proposal.",
        ));
    }
    let request = ConflictResolveRequest {
        memory_a: &receipt.memory_a,
        memory_b: &receipt.memory_b,
        verb: ResolveVerb::parse(&receipt.verb).ok_or_else(|| {
            refusal(
                "conflict_proposal_invalid",
                "Unsupported queued conflict resolution verb.",
            )
        })?,
        keep: receipt.keep.as_deref(),
        reason: &receipt.reason,
        scope_a_tags: receipt.scope_a_tags.clone(),
        scope_b_tags: receipt.scope_b_tags.clone(),
        merge_content: receipt.merge_content.as_deref(),
    };
    let plan = live_plan(connection, &receipt.workspace_id, &request)?;
    if digest(&plan)? != receipt.plan_hash
        || serde_json::to_value(&plan).map_err(storage_error)? != receipt.plan
    {
        return Err(refusal(
            STALE_CODE,
            "The live resolution plan differs from the reviewed proposal.",
        ));
    }
    Ok(CheckedProposal {
        plan,
        reason: receipt.reason,
    })
}

pub(crate) fn apply(
    workspace_path: &std::path::Path,
    database_path: &std::path::Path,
    candidate: &StoredCurationCandidate,
    checked: &CheckedProposal,
    actor: Option<&str>,
) -> Result<crate::core::decide::ConflictResolutionReport, DomainError> {
    let Some(PlannedResolutionAction::RecordDecision {
        topic,
        chosen,
        alternatives,
        supersedes,
    }) = checked
        .plan
        .actions
        .iter()
        .find(|action| matches!(action, PlannedResolutionAction::RecordDecision { .. }))
    else {
        return Err(refusal(
            "conflict_proposal_invalid",
            "A reviewed resolution must contain its decision rationale.",
        ));
    };
    crate::core::decide::decide_conflict_candidate_resolution(
        &crate::core::decide::DecideRecordOptions {
            workspace_path,
            database_path: Some(database_path),
            topic,
            chosen,
            alternatives: alternatives.clone(),
            rationale: &checked.reason,
            revisit_by: None,
            supersedes: supersedes.as_deref(),
            dry_run: false,
            actor,
            now: None,
        },
        &checked.plan,
        candidate,
    )
}

/// Recheck the exact reviewed row under the decision writer lock, before any
/// source mutation. Accept/reject races and receipt edits must abort the writer.
pub(crate) fn validate_in_transaction(
    connection: &DbConnection,
    expected: &StoredCurationCandidate,
    plan: &ConflictResolutionPlan,
    reason: &str,
) -> Result<(), DomainError> {
    let current = connection
        .get_curation_candidate(&expected.workspace_id, &expected.id)
        .map_err(storage_error)?;
    if current.as_ref() != Some(expected) {
        return Err(refusal(
            STALE_CODE,
            "The conflict proposal changed after application was prepared.",
        ));
    }
    let checked = validate_candidate(connection, expected, true)?;
    if &checked.plan != plan || checked.reason != reason {
        return Err(refusal(
            STALE_CODE,
            "The reviewed conflict plan or rationale changed before commit.",
        ));
    }
    Ok(())
}

/// Runs only after all conflict effects succeeded, in the same transaction.
pub(crate) fn complete_in_transaction(
    connection: &DbConnection,
    candidate: &StoredCurationCandidate,
    decision_id: &str,
    merged_memory_id: Option<&str>,
    at: &str,
    actor: Option<&str>,
) -> Result<String, DomainError> {
    let receipt = decode_receipt(candidate)?;
    if (receipt.verb == "merge") != merged_memory_id.is_some()
        || merged_memory_id.is_some_and(|id| id == decision_id || id.parse::<MemoryId>().is_err())
    {
        return Err(refusal(
            "conflict_proposal_invalid",
            "Conflict completion does not match the reviewed output identities.",
        ));
    }
    if !connection
        .mark_curation_candidate_applied(&candidate.workspace_id, &candidate.id, at)
        .map_err(storage_error)?
    {
        return Err(refusal(
            STALE_CODE,
            "The approved conflict proposal could not be completed.",
        ));
    }
    let audit_id = generate_audit_id();
    let mut details = json!({"schema": RECEIPT_SCHEMA, "candidateId": candidate.id, "decision": "resolve_conflict", "fromStatus": "approved", "toStatus": "applied", "resolutionDecisionId": decision_id, "sourceId": candidate.source_id});
    if let Some(memory_id) = merged_memory_id {
        details["mergedMemoryId"] = json!(memory_id);
    }
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(candidate.workspace_id.clone()),
                actor: actor.map(str::to_owned),
                action: audit_actions::CURATION_CANDIDATE_APPLY.to_owned(),
                target_type: Some("curation_candidate".to_owned()),
                target_id: Some(candidate.id.clone()),
                details: Some(details.to_string()),
            },
        )
        .map_err(storage_error)?;
    Ok(audit_id)
}

pub(crate) fn is_merge_candidate(candidate: &StoredCurationCandidate) -> bool {
    decode_receipt(candidate).is_ok_and(|receipt| receipt.verb == "merge")
}

/// Stable publication identity lets a completed apply retry indexing without
/// repeating source mutation or creating another summary.
pub(crate) fn merge_index_job_id(memory_id: &str) -> String {
    let hash = blake3::hash(format!("conflict-merge|{memory_id}").as_bytes())
        .to_hex()
        .to_string();
    format!("sidx_{}", &hash[..26])
}

/// Replaying an applied merge reports the canonical output recorded by the
/// same atomic completion that owns its source effects. It never creates a
/// replacement or substitutes the independently recorded rationale memory.
pub(crate) fn applied_merge_memory_id(
    connection: &DbConnection,
    candidate: &StoredCurationCandidate,
) -> Result<Option<String>, DomainError> {
    let receipt = decode_receipt(candidate)?;
    if receipt.verb != "merge" {
        return Ok(None);
    }
    let invalid = || {
        refusal(
            "conflict_merge_completion_invalid",
            "The applied merge does not have one matching durable output receipt; no merge was repeated.",
        )
    };
    if candidate.status != "applied" || candidate.applied_at.is_none() {
        return Err(invalid());
    }
    let rows = connection
        .query(
            "SELECT details FROM audit_log WHERE workspace_id = ?1 AND action = ?2 AND target_type = 'curation_candidate' AND target_id = ?3 ORDER BY id LIMIT 2",
            &[
                DbValue::Text(candidate.workspace_id.clone()),
                DbValue::Text(audit_actions::CURATION_CANDIDATE_APPLY.to_owned()),
                DbValue::Text(candidate.id.clone()),
            ],
        )
        .map_err(storage_error)?;
    let [row] = rows.as_slice() else {
        return Err(invalid());
    };
    let details: serde_json::Value =
        serde_json::from_str(row.get(0).and_then(DbValue::as_str).ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
    let memory_id = details
        .get("mergedMemoryId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(invalid)?;
    let decision_id = details
        .get("resolutionDecisionId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(invalid)?;
    if details.get("schema").and_then(serde_json::Value::as_str) != Some(RECEIPT_SCHEMA)
        || details
            .get("candidateId")
            .and_then(serde_json::Value::as_str)
            != Some(candidate.id.as_str())
        || details.get("sourceId").and_then(serde_json::Value::as_str)
            != candidate.source_id.as_deref()
        || details.get("decision").and_then(serde_json::Value::as_str) != Some("resolve_conflict")
        || decision_id == memory_id
        || decision_id.parse::<MemoryId>().is_err()
        || memory_id.parse::<MemoryId>().is_err()
    {
        return Err(invalid());
    }
    let memory = connection
        .get_memory(memory_id)
        .map_err(storage_error)?
        .ok_or_else(invalid)?;
    if memory.workspace_id != candidate.workspace_id {
        return Err(invalid());
    }
    let decision = connection
        .get_memory(decision_id)
        .map_err(storage_error)?
        .ok_or_else(invalid)?;
    if decision.workspace_id != candidate.workspace_id {
        return Err(invalid());
    }
    // Completion may only point at this proposal's original output. The
    // current body can legitimately evolve later, so bind immutable creation
    // provenance instead of comparing mutable content or trust fields.
    let creations = connection
        .query(
            "SELECT details FROM audit_log WHERE workspace_id = ?1 AND action = ?2 AND target_type = 'memory' AND target_id = ?3 ORDER BY id LIMIT 2",
            &[
                DbValue::Text(candidate.workspace_id.clone()),
                DbValue::Text(audit_actions::MEMORY_CREATE.to_owned()),
                DbValue::Text(memory_id.to_owned()),
            ],
        )
        .map_err(storage_error)?;
    let [creation] = creations.as_slice() else {
        return Err(invalid());
    };
    let creation: serde_json::Value = serde_json::from_str(
        creation
            .get(0)
            .and_then(DbValue::as_str)
            .ok_or_else(invalid)?,
    )
    .map_err(|_| invalid())?;
    if creation["schema"] != "ee.conflict.merge.v1"
        || creation["candidateId"] != candidate.id
        || creation["sourceId"] != json!(candidate.source_id)
        || creation["mergedMemoryId"] != memory_id
        || creation["resolutionDecisionId"] != decision_id
        || creation["sourceMemoryIds"] != json!([receipt.memory_a, receipt.memory_b])
    {
        return Err(invalid());
    }
    Ok(Some(memory_id.to_owned()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::db::{
        CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, MemoryLinkRelation,
        MemoryLinkSource,
    };
    use sqlmodel_core::Value;

    const WORKSPACE: &str = "wsp_00000000000000000000000072";
    const MEMORY_A: &str = "mem_00000000000000000000000001";
    const MEMORY_B: &str = "mem_00000000000000000000000002";

    fn fixture() -> (DbConnection, StoredCurationCandidate) {
        let connection = DbConnection::open_memory().unwrap();
        connection.migrate().unwrap();
        connection
            .insert_workspace(
                WORKSPACE,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-conflict-proposal-fixture".to_owned(),
                    name: None,
                },
            )
            .unwrap();
        for (id, content) in [
            (MEMORY_A, "The deployment uses configuration alpha."),
            (MEMORY_B, "The deployment uses configuration beta."),
        ] {
            connection
                .insert_memory(
                    id,
                    &CreateMemoryInput {
                        workspace_id: WORKSPACE.to_owned(),
                        level: "semantic".to_owned(),
                        kind: "fact".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence: 0.8,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: None,
                        trust_class: "agent_assertion".to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .unwrap();
        }
        connection
            .insert_memory_link(
                "link_00000000000000000000000001",
                &CreateMemoryLinkInput {
                    src_memory_id: MEMORY_A.to_owned(),
                    dst_memory_id: MEMORY_B.to_owned(),
                    relation: MemoryLinkRelation::Contradicts,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: false,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("conflict-proposal-test".to_owned()),
                    metadata_json: None,
                },
            )
            .unwrap();
        let report = create_in_transaction(
            &connection,
            WORKSPACE,
            &ConflictResolveRequest {
                memory_a: MEMORY_A,
                memory_b: MEMORY_B,
                verb: ResolveVerb::BothValid,
                keep: None,
                reason: "These configurations remain valid in different contexts.",
                scope_a_tags: Vec::new(),
                scope_b_tags: Vec::new(),
                merge_content: None,
            },
            Some("test"),
        )
        .unwrap();
        let candidate = connection
            .get_curation_candidate(WORKSPACE, &report.candidate_id)
            .unwrap()
            .unwrap();
        (connection, candidate)
    }

    #[test]
    fn unknown_receipt_versions_and_retyped_payloads_never_enter_generic_link_apply() {
        let (connection, candidate) = fixture();
        let mut unknown_source = candidate.clone();
        unknown_source.source_id = Some("conflict-resolution:v2:unsupported".to_owned());
        let mut unknown_schema = candidate.clone();
        unknown_schema.source_id = Some("unrelated-source".to_owned());
        let mut payload: serde_json::Value =
            serde_json::from_str(unknown_schema.proposed_content.as_deref().unwrap()).unwrap();
        payload["schema"] = json!("ee.conflict.proposal_receipt.v2");
        unknown_schema.proposed_content = Some(payload.to_string());
        let mut retyped = candidate;
        retyped.candidate_type = "tombstone".to_owned();
        for altered in [unknown_source, unknown_schema, retyped] {
            assert!(is_candidate(&altered));
            assert_eq!(
                validate_candidate(&connection, &altered, false)
                    .unwrap_err()
                    .code(),
                "conflict_proposal_invalid"
            );
        }
    }

    #[test]
    fn scope_split_rejects_unstorable_tags_without_queue_or_resolution_writes() {
        let (connection, _) = fixture();
        let before_candidates = connection.count_table_rows("curation_candidates").unwrap();
        let before_audits = connection.count_table_rows("audit_log").unwrap();
        let before_memories = connection.count_table_rows("memories").unwrap();
        for invalid_tag in [
            "x".repeat(65),
            "scope/team".to_owned(),
            "scope team".to_owned(),
            "é".repeat(33),
        ] {
            let result = create_in_transaction(
                &connection,
                WORKSPACE,
                &ConflictResolveRequest {
                    memory_a: MEMORY_A,
                    memory_b: MEMORY_B,
                    verb: ResolveVerb::ScopeSplit,
                    keep: None,
                    reason: "These configurations have explicit deployment scopes.",
                    scope_a_tags: vec![invalid_tag],
                    scope_b_tags: vec!["development".to_owned()],
                    merge_content: None,
                },
                Some("test"),
            );
            assert_eq!(result.unwrap_err().code(), "conflict_proposal_invalid");
            assert_eq!(
                connection.count_table_rows("curation_candidates").unwrap(),
                before_candidates
            );
            assert_eq!(
                connection.count_table_rows("audit_log").unwrap(),
                before_audits
            );
            assert_eq!(
                connection.count_table_rows("memories").unwrap(),
                before_memories
            );
            assert!(connection.get_memory_tags(MEMORY_A).unwrap().is_empty());
            assert!(connection.get_memory_tags(MEMORY_B).unwrap().is_empty());
        }
    }

    #[test]
    fn scope_split_reviews_canonical_tags_and_reuses_equivalent_requests() {
        let (connection, _) = fixture();
        let request = ConflictResolveRequest {
            memory_a: MEMORY_A,
            memory_b: MEMORY_B,
            verb: ResolveVerb::ScopeSplit,
            keep: None,
            reason: "These configurations have explicit deployment scopes.",
            scope_a_tags: vec![" Production ".to_owned(), "PRODUCTION".to_owned()],
            scope_b_tags: vec!["Development".to_owned()],
            merge_content: None,
        };
        let proposed =
            create_in_transaction(&connection, WORKSPACE, &request, Some("test")).unwrap();
        let repeated = create_in_transaction(
            &connection,
            WORKSPACE,
            &ConflictResolveRequest {
                scope_a_tags: vec!["production".to_owned()],
                scope_b_tags: vec!["development".to_owned()],
                ..request
            },
            Some("test"),
        )
        .unwrap();
        assert!(proposed.created && !repeated.created);
        assert_eq!(proposed.candidate_id, repeated.candidate_id);
        assert_eq!(proposed.plan, repeated.plan);
        let candidate = connection
            .get_curation_candidate(WORKSPACE, &proposed.candidate_id)
            .unwrap()
            .unwrap();
        let mut receipt = decode_receipt(&candidate).unwrap();
        assert_eq!(receipt.scope_a_tags, vec!["production".to_owned()]);
        assert_eq!(receipt.scope_b_tags, vec!["development".to_owned()]);
        validate_candidate(&connection, &candidate, false).unwrap();

        // Even a recomputed producer commitment cannot make a noncanonical
        // scope eligible for application through the reserved queue producer.
        receipt.scope_a_tags = vec!["Production".to_owned()];
        let receipt_hash = digest(&receipt).unwrap();
        let mut altered = candidate;
        altered.id = candidate_id(&receipt_hash);
        altered.source_id = Some(format!("{SOURCE_PREFIX}{receipt_hash}"));
        altered.proposed_content = Some(serde_json::to_string(&receipt).unwrap());
        assert_eq!(
            validate_receipt(&altered).unwrap_err().code(),
            "conflict_proposal_invalid"
        );
    }

    #[test]
    fn revision_closure_invalidates_a_proposal_even_with_an_identical_stored_body() {
        let (connection, candidate) = fixture();
        validate_candidate(&connection, &candidate, false).unwrap();
        let before = connection.get_memory(MEMORY_A).unwrap();
        connection
            .execute(
                "UPDATE memories SET superseded_at = ?1 WHERE id = ?2",
                &[
                    Value::Text("2026-01-01T00:00:00Z".to_owned()),
                    Value::Text(MEMORY_A.to_owned()),
                ],
            )
            .unwrap();
        assert_eq!(connection.get_memory(MEMORY_A).unwrap(), before);
        assert_eq!(
            validate_candidate(&connection, &candidate, false)
                .unwrap_err()
                .code(),
            STALE_CODE
        );
    }

    #[test]
    fn merge_receipts_bind_canonical_content_and_equivalent_requests_are_idempotent() {
        let (connection, _) = fixture();
        let request = ConflictResolveRequest {
            memory_a: MEMORY_A,
            memory_b: MEMORY_B,
            verb: ResolveVerb::Merge,
            keep: None,
            reason: "The configurations are combined into one reviewed summary.",
            scope_a_tags: Vec::new(),
            scope_b_tags: Vec::new(),
            merge_content: Some("  Production uses alpha; local checks use beta.  "),
        };
        let first = create_in_transaction(&connection, WORKSPACE, &request, Some("test")).unwrap();
        let repeated = create_in_transaction(
            &connection,
            WORKSPACE,
            &ConflictResolveRequest {
                memory_a: MEMORY_B,
                memory_b: MEMORY_A,
                merge_content: Some("Production uses alpha; local checks use beta."),
                ..request
            },
            Some("test"),
        )
        .unwrap();
        assert!(first.created && !repeated.created);
        assert_eq!(first.candidate_id, repeated.candidate_id);
        let candidate = connection
            .get_curation_candidate(WORKSPACE, &first.candidate_id)
            .unwrap()
            .unwrap();
        let receipt = decode_receipt(&candidate).unwrap();
        assert_eq!(
            receipt.merge_content.as_deref(),
            Some("Production uses alpha; local checks use beta.")
        );
        assert_eq!(receipt.memory_a, first.plan.memory_a);
        assert_eq!(receipt.memory_b, first.plan.memory_b);
        validate_candidate(&connection, &candidate, false).unwrap();
    }

    #[test]
    fn merge_source_scope_type_and_lifetime_refusals_do_not_write_proposals() {
        for change in [
            "workflow",
            "tags",
            "typed",
            "procedural",
            "specialized",
            "future",
            "expired",
        ] {
            let (connection, _) = fixture();
            match change {
                "tags" => connection
                    .add_memory_tags(MEMORY_A, &["global".to_owned()])
                    .unwrap(),
                "typed" => {
                    // A legacy or damaged sidecar must not be discarded by
                    // summary synthesis even though new fact writes reject it.
                    connection
                        .execute(
                            "UPDATE memories SET typed_fields_json = ?1 WHERE id = ?2",
                            &[
                                Value::Text(r#"{"scope":"specialized"}"#.to_owned()),
                                Value::Text(MEMORY_A.to_owned()),
                            ],
                        )
                        .unwrap();
                }
                _ => {
                    let (field, value) = match change {
                        "workflow" => ("workflow_id", "deployment"),
                        "procedural" => ("level", "procedural"),
                        "specialized" => ("kind", "risk"),
                        "future" => ("valid_from", "2999-01-01T00:00:00Z"),
                        "expired" => ("valid_to", "2000-01-01T00:00:00Z"),
                        _ => unreachable!(),
                    };
                    connection
                        .execute_raw(&format!(
                            "UPDATE memories SET {field} = '{value}' WHERE id = '{MEMORY_A}'"
                        ))
                        .unwrap();
                }
            }
            let before_candidates = connection.count_table_rows("curation_candidates").unwrap();
            let before_audits = connection.count_table_rows("audit_log").unwrap();
            let before_a = connection.get_memory(MEMORY_A).unwrap();
            let before_b = connection.get_memory(MEMORY_B).unwrap();
            let result = create_in_transaction(
                &connection,
                WORKSPACE,
                &ConflictResolveRequest {
                    memory_a: MEMORY_A,
                    memory_b: MEMORY_B,
                    verb: ResolveVerb::Merge,
                    keep: None,
                    reason: "The configurations require an explicit reviewed summary.",
                    scope_a_tags: Vec::new(),
                    scope_b_tags: Vec::new(),
                    merge_content: Some("Production uses alpha; local checks use beta."),
                },
                Some("test"),
            );
            assert!(result.is_err(), "{change}");
            assert_eq!(
                connection.count_table_rows("curation_candidates").unwrap(),
                before_candidates,
                "{change}"
            );
            assert_eq!(
                connection.count_table_rows("audit_log").unwrap(),
                before_audits,
                "{change}"
            );
            assert_eq!(
                connection.get_memory(MEMORY_A).unwrap(),
                before_a,
                "{change}"
            );
            assert_eq!(
                connection.get_memory(MEMORY_B).unwrap(),
                before_b,
                "{change}"
            );
        }
    }
}
