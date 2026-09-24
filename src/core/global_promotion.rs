//! Workspace → global promotion/demotion decision core (bd-1bfwa.2).
//!
//! Pure, deterministic policy: given a workspace memory row and the relevant
//! global-store context, decide whether promotion is allowed, refused, or a
//! duplicate-merge, with the redaction outcome and an audit preview. No I/O —
//! execution (store writes, audit rows, backflow) lives in a later slice so
//! this core stays trivially testable and reusable by both the engine and the
//! `.3` CLI plan output.

use serde_json::{Value, json};

use crate::policy::redact_secret_like_content;

#[path = "global_promotion_admission.rs"]
mod admission;

#[path = "global_promotion_payload.rs"]
mod payload;

use payload::PromotionPayload;

pub const GLOBAL_PROMOTION_PLAN_SCHEMA_V1: &str = "ee.global_promotion.plan.v1";

/// Degraded/refusal code emitted when secret-like content blocks promotion.
/// A workspace memory may legitimately hold workspace-scoped secret mentions
/// (with `--allow-secret-mention`); the global tier crosses workspace
/// boundaries, so promotion re-screens and refuses rather than redacting
/// silently (no silent memory mutation).
pub const GLOBAL_PROMOTION_REDACTION_REFUSED_CODE: &str = "global_promotion_redaction_refused";

/// Trust classes strong enough to cross the workspace boundary. Promotion is
/// an evidence gate, not a convenience: agent assertions and raw imports stay
/// workspace-local until validated (ADR 0081 / bd-1bfwa.2).
const PROMOTABLE_TRUST_CLASSES: [&str; 2] = ["human_explicit", "agent_validated"];

/// The memory-row facts the decision consumes. Deliberately a narrow
/// projection of `StoredMemory` so the core cannot depend on storage types.
#[derive(Clone, Debug)]
pub struct PromotionCandidate {
    pub memory_id: String,
    pub workspace_id: String,
    pub content: String,
    pub level: String,
    pub kind: String,
    pub trust_class: String,
    pub confidence: f32,
    pub tombstoned: bool,
    /// Durable seal-sidecar state resolved by the storage boundary.
    pub sealed: bool,
}

/// An existing global-store row that content-matches the candidate closely
/// enough that promotion should reinforce it instead of inserting a twin.
#[derive(Clone, Debug)]
pub struct GlobalNearDuplicate {
    pub global_memory_id: String,
    /// Similarity in `0.0..=1.0` as computed by the caller's near-duplicate
    /// machinery (the same scorer `remember --reinforce` uses).
    pub similarity: f32,
}

/// Similarity at or above which promotion merges into the existing global
/// row rather than creating a sibling. Matches the `[curation]
/// duplicate_similarity` default used by `remember --reinforce`.
pub const DEFAULT_PROMOTION_MERGE_SIMILARITY: f32 = 0.92;

#[derive(Clone, Debug)]
pub struct PromotionInput {
    pub candidate: PromotionCandidate,
    /// Closest existing global row, if the caller found one.
    pub nearest_global_duplicate: Option<GlobalNearDuplicate>,
    /// Merge threshold override; `None` uses the default.
    pub merge_similarity: Option<f32>,
    /// Whether the global tier is enabled and this workspace participates.
    pub global_lane_available: bool,
}

/// Why a promotion was refused. Every variant carries enough to render an
/// honest, actionable error without re-deriving the decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PromotionRefusal {
    /// The global tier is disabled or this workspace opted out.
    LaneUnavailable,
    /// The memory is tombstoned; dead rows do not cross the boundary.
    Tombstoned,
    /// Sealed memory: the body is withheld pending reveal.
    SealedPlaceholder,
    /// A replaced revision cannot be promoted as a new global head.
    Superseded,
    /// The source's authored validity window has not opened yet.
    NotYetValid,
    /// The source's authored validity window has closed.
    Expired,
    /// Trust class below the evidence gate.
    EvidenceGateTrustTooLow { trust_class: String },
    /// Current attempt-family evidence no longer permits promotion.
    AttemptFamily {
        posture: crate::models::AttemptFamilyPromotionPosture,
    },
    /// Secret-like content detected; promotion refuses rather than redacts.
    RedactionRefused { reasons: Vec<&'static str> },
}

impl PromotionRefusal {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::LaneUnavailable => "global_lane_unavailable",
            Self::Tombstoned => "global_promotion_tombstoned",
            Self::SealedPlaceholder => "global_promotion_sealed",
            Self::Superseded => "global_promotion_superseded",
            Self::NotYetValid => "global_promotion_not_yet_valid",
            Self::Expired => "global_promotion_expired",
            Self::EvidenceGateTrustTooLow { .. } | Self::AttemptFamily { .. } => {
                "global_promotion_evidence_gate"
            }
            Self::RedactionRefused { .. } => GLOBAL_PROMOTION_REDACTION_REFUSED_CODE,
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::LaneUnavailable => {
                "The global memory lane is disabled or this workspace does not participate."
                    .to_owned()
            }
            Self::Tombstoned => {
                "Tombstoned memories cannot be promoted to the global lane.".to_owned()
            }
            Self::SealedPlaceholder => {
                "Sealed memories cannot be promoted until revealed; the global lane never carries withheld-content placeholders."
                    .to_owned()
            }
            Self::Superseded => {
                "Superseded revisions cannot become current global memories.".to_owned()
            }
            Self::NotYetValid => {
                "The source memory is not yet valid for global promotion.".to_owned()
            }
            Self::Expired => {
                "Expired memories cannot be promoted as current global knowledge.".to_owned()
            }
            Self::EvidenceGateTrustTooLow { trust_class } => format!(
                "Promotion requires trust class human_explicit or agent_validated; this memory is `{trust_class}`."
            ),
            Self::AttemptFamily { posture } => format!(
                "Attempt-family evidence blocks global promotion ({}): {}.",
                posture.as_str(), posture.reason()
            ),
            Self::RedactionRefused { reasons } => format!(
                "Secret-like content blocks promotion across workspace boundaries ({}); promotion refuses rather than silently redacting.",
                reasons.join(", ")
            ),
        }
    }

    #[must_use]
    pub fn repair(&self) -> String {
        match self {
            Self::LaneUnavailable => {
                "Enable `[memory] include_global` and workspace participation, then retry."
                    .to_owned()
            }
            Self::Tombstoned => "Promote an active memory instead.".to_owned(),
            Self::SealedPlaceholder => {
                "Reveal the memory first: ee memory reveal <id> --content-file <path> --json"
                    .to_owned()
            }
            Self::Superseded | Self::Expired => {
                "Promote a current, active revision instead.".to_owned()
            }
            Self::NotYetValid => {
                "Retry after the authored validity window opens.".to_owned()
            }
            Self::EvidenceGateTrustTooLow { .. } => {
                "Validate the memory first (record outcome evidence or human confirmation), then retry."
                    .to_owned()
            }
            Self::AttemptFamily { .. } => {
                "Record the actual sibling attempts and resolve family conflicts before retrying; a stored trust label does not bypass incomplete evidence.".to_owned()
            }
            Self::RedactionRefused { .. } => {
                "Remove or externalize the secret-like content, re-remember, and promote the clean row."
                    .to_owned()
            }
        }
    }
}

/// The action a permitted promotion will take.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PromotionAction {
    /// Insert a new global row carrying origin provenance.
    Insert,
    /// Reinforce the existing near-duplicate global row instead of
    /// inserting a twin (dup-merge-at-promotion).
    MergeInto { global_memory_id: String },
}

#[derive(Clone, Debug)]
pub enum PromotionVerdict {
    Allow { action: PromotionAction },
    Refuse { refusal: PromotionRefusal },
}

/// Deterministic plan for one promotion. Serializable for `--dry-run`
/// surfaces and reused verbatim by the execution slice.
#[derive(Clone, Debug)]
pub struct PromotionPlan {
    pub memory_id: String,
    pub origin_workspace_id: String,
    pub verdict: PromotionVerdict,
    /// Audit action the execution slice will record on success.
    pub audit_action: &'static str,
}

impl PromotionPlan {
    #[must_use]
    pub fn allowed(&self) -> bool {
        matches!(self.verdict, PromotionVerdict::Allow { .. })
    }

    #[must_use]
    pub fn data_json(&self) -> Value {
        let (verdict, detail) = match &self.verdict {
            PromotionVerdict::Allow {
                action: PromotionAction::Insert,
            } => ("allow", json!({ "action": "insert" })),
            PromotionVerdict::Allow {
                action: PromotionAction::MergeInto { global_memory_id },
            } => (
                "allow",
                json!({ "action": "merge_into", "globalMemoryId": global_memory_id }),
            ),
            PromotionVerdict::Refuse { refusal } => (
                "refuse",
                json!({
                    "code": refusal.code(),
                    "message": refusal.message(),
                    "repair": refusal.repair(),
                }),
            ),
        };
        json!({
            "schema": GLOBAL_PROMOTION_PLAN_SCHEMA_V1,
            "memoryId": self.memory_id,
            "originWorkspaceId": self.origin_workspace_id,
            "verdict": verdict,
            "detail": detail,
            "auditAction": self.audit_action,
        })
    }
}

/// Decide one promotion. Pure; the caller supplies near-duplicate context.
#[must_use]
pub fn plan_promotion(input: &PromotionInput) -> PromotionPlan {
    let candidate = &input.candidate;
    let refuse = |refusal: PromotionRefusal| PromotionPlan {
        memory_id: candidate.memory_id.clone(),
        origin_workspace_id: candidate.workspace_id.clone(),
        verdict: PromotionVerdict::Refuse { refusal },
        audit_action: "memory.promote_global_refused",
    };

    if !input.global_lane_available {
        return refuse(PromotionRefusal::LaneUnavailable);
    }
    if candidate.tombstoned {
        return refuse(PromotionRefusal::Tombstoned);
    }
    if candidate.sealed {
        return refuse(PromotionRefusal::SealedPlaceholder);
    }
    if !PROMOTABLE_TRUST_CLASSES.contains(&candidate.trust_class.as_str()) {
        return refuse(PromotionRefusal::EvidenceGateTrustTooLow {
            trust_class: candidate.trust_class.clone(),
        });
    }
    let redaction = redact_secret_like_content(&candidate.content);
    if redaction.redacted {
        return refuse(PromotionRefusal::RedactionRefused {
            reasons: redaction.redacted_reasons,
        });
    }

    let threshold = input
        .merge_similarity
        .unwrap_or(DEFAULT_PROMOTION_MERGE_SIMILARITY);
    let action = match &input.nearest_global_duplicate {
        Some(duplicate) if duplicate.similarity >= threshold => PromotionAction::MergeInto {
            global_memory_id: duplicate.global_memory_id.clone(),
        },
        _ => PromotionAction::Insert,
    };
    PromotionPlan {
        memory_id: candidate.memory_id.clone(),
        origin_workspace_id: candidate.workspace_id.clone(),
        verdict: PromotionVerdict::Allow { action },
        audit_action: "memory.promote_global",
    }
}

// ── Execution (slice 2) ────────────────────────────────────────────────────
//
// The engine turns an allowed plan into durable state: a copy (never a move)
// of the workspace memory into the separate global store with origin
// provenance, audits in BOTH stores, idempotent re-promotion, and a
// tombstone-based demotion. The decision core above stays pure.

use std::path::Path;

use crate::db::{
    CreateAuditInput, CreateMemoryInput, CreateSearchIndexJobInput, DbConnection,
    SearchIndexJobType, generate_audit_id,
};

pub const GLOBAL_PROMOTION_REPORT_SCHEMA_V1: &str = "ee.global_promotion.report.v1";
pub const GLOBAL_DEMOTION_REPORT_SCHEMA_V1: &str = "ee.global_demotion.report.v1";

/// Feedback-event ids must satisfy the schema CHECK (`fb_` + 26-char
/// payload, length 29); mirror `core::outcome`'s private generator.
fn promotion_feedback_event_id() -> String {
    let memory_id = crate::models::MemoryId::now().to_string();
    let payload = memory_id.trim_start_matches("mem_");
    format!("fb_{payload}")
}

/// Search-index job ids must satisfy the schema CHECK
/// (`sidx_` + 26-char payload, length 31); mirror the private generator in
/// `core::memory` rather than widening its visibility.
fn promotion_index_job_id() -> String {
    let memory_id = crate::models::MemoryId::now().to_string();
    let payload = memory_id.trim_start_matches("mem_");
    format!("sidx_{payload}")
}

/// Provenance URI carried by every promoted global row, binding it to its
/// origin workspace memory: `ee-mem://<workspace_id>/<memory_id>`.
#[must_use]
pub fn promotion_provenance_uri(workspace_id: &str, memory_id: &str) -> String {
    format!("ee-mem://{workspace_id}/{memory_id}")
}

#[derive(Clone, Debug)]
pub struct PromoteGlobalOptions<'a> {
    /// Workspace database holding the memory to promote.
    pub workspace_database_path: &'a Path,
    pub memory_id: &'a str,
    /// Resolved global store paths (callers use
    /// [`super::global_store::default_global_store_paths_from_env`] in
    /// production; tests pass a temp root).
    pub global_paths: &'a super::global_store::GlobalStorePaths,
    /// Whether config enables the lane for this workspace (the CLI slice
    /// resolves this; the engine only enforces it through the plan).
    pub global_lane_available: bool,
    pub actor: Option<&'a str>,
    pub dry_run: bool,
}

#[derive(Clone, Debug)]
pub struct PromotionReport {
    pub plan: PromotionPlan,
    pub executed: bool,
    /// The global row this promotion created or matched.
    pub global_memory_id: Option<String>,
    /// True when an exact-content global twin already existed: the
    /// promotion is a no-op re-promotion (idempotence == merge for the
    /// exact-match case).
    pub already_promoted: bool,
    /// New index job or unfinished receipt recovered for an existing global row.
    pub index_job_id: Option<String>,
    /// Honest derived-index posture: `indexed`, `queued`, `failed`, or
    /// `not_applicable` for refusal, dry-run, or a merge with no outstanding job.
    pub index_status: String,
    /// Derived-index error detail when immediate reconciliation did not
    /// converge. The durable global memory remains committed.
    pub index_error: Option<String>,
}

impl PromotionReport {
    #[must_use]
    pub fn data_json(&self) -> Value {
        json!({
            "schema": GLOBAL_PROMOTION_REPORT_SCHEMA_V1,
            "plan": self.plan.data_json(),
            "executed": self.executed,
            "globalMemoryId": self.global_memory_id,
            "alreadyPromoted": self.already_promoted,
            "indexJobId": self.index_job_id,
            "indexStatus": self.index_status,
            "indexError": self.index_error,
        })
    }
}

fn promotion_audit_details(
    memory: &crate::db::StoredMemory,
    global_id: &str,
    already_promoted: bool,
    payload: &PromotionPayload,
) -> String {
    json!({
        "schema": GLOBAL_PROMOTION_REPORT_SCHEMA_V1,
        "originWorkspaceId": memory.workspace_id,
        "originMemoryId": memory.id,
        "globalMemoryId": global_id,
        "alreadyPromoted": already_promoted,
        "sourcePayload": payload.audit_evidence(),
    })
    .to_string()
}

/// A durable twin does not prove its derived index was published. Recover the
/// oldest unfinished receipt for this exact entity while holding the same
/// transaction as duplicate selection. Never create a second job just because
/// the original publisher crashed or its separate origin audit failed.
///
/// Selection does not steal a running publisher or erase failure evidence.
/// After commit, the existing memory reconciler owns lease-aware recovery,
/// failed/cancelled retries, and authoritative index-status reporting.
fn unfinished_promotion_index_job(
    connection: &DbConnection,
    workspace: &str,
    memory_id: &str,
) -> crate::db::Result<Option<String>> {
    use sqlmodel_core::Value;

    let rows = connection.query(
        "SELECT id, status FROM search_index_jobs WHERE workspace_id = ?1 AND job_type = ?2 AND document_source = ?3 AND document_id = ?4 AND status != ?5 ORDER BY created_at ASC, id ASC LIMIT 1",
        &[
            Value::Text(workspace.to_owned()),
            Value::Text(SearchIndexJobType::SingleDocument.as_str().to_owned()),
            Value::Text("memory".to_owned()),
            Value::Text(memory_id.to_owned()),
            Value::Text(crate::db::SearchIndexJobStatus::Completed.as_str().to_owned()),
        ],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let (Some(Value::Text(id)), Some(Value::Text(status))) = (row.get(0), row.get(1)) else {
        return Err(promotion_index_receipt_error());
    };
    if crate::db::SearchIndexJobStatus::parse(status).is_none() {
        return Err(promotion_index_receipt_error());
    }
    Ok(Some(id.clone()))
}

fn promotion_index_receipt_error() -> crate::db::DbError {
    crate::db::DbError::MalformedRow {
        operation: crate::db::DbOperation::Query,
        message: "Could not verify the existing global promotion index receipt".to_owned(),
    }
}

/// Publish all destination obligations under one transaction. The caller has
/// already admitted a coherent source snapshot; these are separate databases,
/// not a distributed transaction. Index reconciliation happens after commit.
fn persist_global_promotion(
    connection: &DbConnection,
    workspace: &str,
    memory: &crate::db::StoredMemory,
    payload: &PromotionPayload,
    actor: Option<&str>,
    reference: chrono::DateTime<chrono::Utc>,
) -> crate::db::Result<(String, bool, Option<String>)> {
    connection.with_transaction(|| {
        // Select the twin inside the write transaction, not in a snapshot
        // released before publication. Never trust a stale preview decision.
        let twin = admission::find_twin(connection, workspace, memory, payload, reference)?;
        let (id, already_promoted, job) = match twin {
            Some(id) => {
                let job = unfinished_promotion_index_job(connection, workspace, &id)?;
                (id, true, job)
            }
            None => {
                let id = crate::models::MemoryId::now().to_string();
                let job = promotion_index_job_id();
                connection.insert_memory(
                    &id,
                    &CreateMemoryInput {
                        workspace_id: workspace.to_owned(),
                        level: memory.level.clone(),
                        kind: memory.kind.clone(),
                        content: memory.content.clone(),
                        workflow_id: None,
                        confidence: memory.confidence,
                        utility: memory.utility,
                        importance: memory.importance,
                        provenance_uri: Some(promotion_provenance_uri(
                            &memory.workspace_id,
                            &memory.id,
                        )),
                        trust_class: memory.trust_class.clone(),
                        trust_subclass: memory.trust_subclass.clone(),
                        tags: vec![
                            "scope:global".to_owned(),
                            format!("origin:{}", memory.workspace_id),
                        ],
                        valid_from: memory.valid_from.clone(),
                        valid_to: memory.valid_to.clone(),
                    },
                )?;
                // insert_memory(None) defaults to capture time. Restore the
                // source's genuinely unbounded start before committing. The
                // interpolated ID is freshly generated, never supplied SQL.
                if memory.valid_from.is_none() {
                    connection.execute_raw(&format!(
                        "UPDATE memories SET valid_from = NULL WHERE id = '{id}'"
                    ))?;
                }
                // Preserve the validated structured payload before a search job or
                // success receipt can make the new row visible as complete.
                payload.apply(connection, &id)?;
                connection.insert_search_index_job(
                    &job,
                    &CreateSearchIndexJobInput {
                        workspace_id: workspace.to_owned(),
                        job_type: SearchIndexJobType::SingleDocument,
                        document_source: Some("memory".to_owned()),
                        document_id: Some(id.clone()),
                        documents_total: 1,
                    },
                )?;
                (id, false, Some(job))
            }
        };
        connection.insert_audit(
            &generate_audit_id(),
            &CreateAuditInput {
                workspace_id: Some(workspace.to_owned()),
                actor: actor.map(str::to_owned),
                action: "memory.promote_global".to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some(id.clone()),
                details: Some(promotion_audit_details(
                    memory,
                    &id,
                    already_promoted,
                    payload,
                )),
            },
        )?;
        Ok((id, already_promoted, job))
    })
}

/// Promote one workspace memory into the user-global store.
///
/// # Errors
///
/// Returns a human-readable error string when storage access fails or the
/// memory does not exist; policy refusals are NOT errors — they come back
/// as a report whose plan carries the refusal (typed code/message/repair)
/// so callers render them honestly without string-matching.
pub fn promote_global(options: &PromoteGlobalOptions<'_>) -> Result<PromotionReport, String> {
    let reference = chrono::Utc::now();
    let (memory, payload, mut plan) = admission::load_source(options, reference)?;
    // Refusals do not inspect, initialize, migrate, or repair the global store.
    if !plan.allowed() {
        return Ok(admission::preview_report(plan, None));
    }
    if options.dry_run {
        let twin = admission::preview_twin(options.global_paths, &memory, &payload, reference)?;
        admission::set_duplicate(&mut plan, twin.as_deref());
        return Ok(admission::preview_report(plan, twin));
    }

    let workspace_connection = DbConnection::open_file(options.workspace_database_path)
        .map_err(|error| format!("open workspace database for audit: {error}"))?;
    let (global_connection, global_workspace_id) =
        super::global_store::open_or_create_global_store(options.global_paths)
            .map_err(|error| format!("open global store: {error}"))?;
    // Destination state is one durable unit; a preview cannot reserve a twin.
    let (global_memory_id, already_promoted, index_job_id) = persist_global_promotion(
        &global_connection,
        &global_workspace_id,
        &memory,
        &payload,
        options.actor,
        reference,
    )
    .map_err(|_| {
        "Global promotion transaction failed; inspect the destination before retrying".to_owned()
    })?;
    if already_promoted {
        admission::set_duplicate(&mut plan, Some(&global_memory_id));
    }

    // Separate databases cannot share this transaction. Report committed state
    // explicitly if the origin audit fails; a compatible retry repairs the
    // audit without inserting another destination memory or index job.
    let origin_audit = workspace_connection
        .insert_audit(
            &generate_audit_id(),
            &CreateAuditInput {
                workspace_id: Some(memory.workspace_id.clone()),
                actor: options.actor.map(str::to_owned),
                action: plan.audit_action.to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some(memory.id.clone()),
                details: Some(promotion_audit_details(
                    &memory,
                    &global_memory_id,
                    already_promoted,
                    &payload,
                )),
            },
        )
        .map_err(|_| {
            format!(
                "global_promotion_origin_audit_pending: global memory {global_memory_id} is committed; retry promotion to repair the origin audit"
            )
        });
    let (index_status, index_error) = index_job_id.as_ref().map_or_else(
        || ("not_applicable".to_owned(), None),
        |index_job_id| {
            let report = super::memory::reconcile_committed_memory_index_job(
                &global_connection,
                &global_workspace_id,
                index_job_id,
                &options.global_paths.index_dir,
            );
            let provisional_status = super::memory::remember_index_status(&report);
            let status = super::memory::authoritative_remember_index_status(
                &global_workspace_id,
                &options.global_paths.root,
                &options.global_paths.database_path,
                &options.global_paths.index_dir,
                std::slice::from_ref(index_job_id),
                &provisional_status,
            );
            (status, report.error)
        },
    );
    // The destination has committed. Its index publication and connection
    // cleanup remain obligations even when the separate origin audit failed.
    // A later retry also recovers this same receipt if publication is deferred.
    let _ = global_connection.close();
    let _ = workspace_connection.close();
    origin_audit?;

    Ok(PromotionReport {
        plan,
        executed: true,
        global_memory_id: Some(global_memory_id),
        already_promoted,
        index_job_id,
        index_status,
        index_error,
    })
}

#[derive(Clone, Debug)]
pub struct DemoteGlobalOptions<'a> {
    /// Workspace database used for the origin-side audit trail.
    pub workspace_database_path: &'a Path,
    /// The GLOBAL memory id to demote.
    pub global_memory_id: &'a str,
    pub global_paths: &'a super::global_store::GlobalStorePaths,
    pub actor: Option<&'a str>,
    pub dry_run: bool,
}

#[derive(Clone, Debug)]
pub struct DemotionReport {
    pub global_memory_id: String,
    pub executed: bool,
    pub tombstoned: bool,
    /// Origin parsed back from the global row's promotion provenance, when
    /// the row was created by `promote_global`.
    pub origin: Option<(String, String)>,
    pub index_job_id: Option<String>,
    pub index_status: String,
    pub index_error: Option<String>,
}

impl DemotionReport {
    #[must_use]
    pub fn data_json(&self) -> Value {
        json!({
            "schema": GLOBAL_DEMOTION_REPORT_SCHEMA_V1,
            "globalMemoryId": self.global_memory_id,
            "executed": self.executed,
            "tombstoned": self.tombstoned,
            "originWorkspaceId": self.origin.as_ref().map(|(workspace, _)| workspace.clone()),
            "originMemoryId": self.origin.as_ref().map(|(_, memory)| memory.clone()),
            "indexJobId": self.index_job_id,
            "indexStatus": self.index_status,
            "indexError": self.index_error,
        })
    }
}

/// Parse a promotion provenance URI back into `(workspace_id, memory_id)`.
#[must_use]
pub fn parse_promotion_provenance(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("ee-mem://")?;
    let (workspace, memory) = rest.split_once('/')?;
    (!workspace.is_empty() && !memory.is_empty()).then(|| (workspace.to_owned(), memory.to_owned()))
}

fn global_mutation_error(message: &'static str) -> crate::db::DbError {
    crate::db::DbError::MalformedRow {
        operation: crate::db::DbOperation::Query,
        message: message.to_owned(),
    }
}

/// Reload authority in the write transaction. A caller's earlier preview does
/// not authorize a row that has moved to another workspace in the meantime.
fn global_mutation_target(
    connection: &DbConnection,
    workspace: &str,
    id: &str,
) -> crate::db::Result<crate::db::StoredMemory> {
    if id.parse::<crate::models::MemoryId>().is_err() {
        return Err(global_mutation_error("Invalid global memory identity"));
    }
    let memory = connection
        .get_memory(id)?
        .ok_or_else(|| global_mutation_error("Global memory not found"))?;
    if memory.workspace_id != workspace {
        return Err(global_mutation_error("Global memory workspace mismatch"));
    }
    Ok(memory)
}

fn demotion_audit_details(memory: &crate::db::StoredMemory) -> String {
    let origin = memory
        .provenance_uri
        .as_deref()
        .and_then(parse_promotion_provenance);
    json!({
        "schema": GLOBAL_DEMOTION_REPORT_SCHEMA_V1,
        "globalMemoryId": memory.id,
        "originWorkspaceId": origin.as_ref().map(|(workspace, _)| workspace),
        "originMemoryId": origin.as_ref().map(|(_, memory)| memory),
    })
    .to_string()
}

/// Commit withdrawal, its index repair, and its destination audit together.
/// Repeating a withdrawal keeps the original tombstone timestamp but queues a
/// fresh repair: an earlier process may have died before reconciling its job.
fn persist_global_demotion(
    connection: &DbConnection,
    workspace: &str,
    id: &str,
    actor: Option<&str>,
) -> crate::db::Result<(crate::db::StoredMemory, bool, String)> {
    connection.with_transaction(|| {
        let memory = global_mutation_target(connection, workspace, id)?;
        let changed = memory.tombstoned_at.is_none();
        if changed && !connection.tombstone_memory(id)? {
            return Err(global_mutation_error("Global withdrawal was not applied"));
        }
        let job = promotion_index_job_id();
        connection.insert_search_index_job(
            &job,
            &CreateSearchIndexJobInput {
                workspace_id: workspace.to_owned(),
                job_type: SearchIndexJobType::SingleDocument,
                document_source: Some("memory".to_owned()),
                document_id: Some(memory.id.clone()),
                documents_total: 1,
            },
        )?;
        connection.insert_audit(
            &generate_audit_id(),
            &CreateAuditInput {
                workspace_id: Some(workspace.to_owned()),
                actor: actor.map(str::to_owned),
                action: "memory.demote_global".to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some(memory.id.clone()),
                details: Some(demotion_audit_details(&memory)),
            },
        )?;
        Ok((memory, changed, job))
    })
}

/// The separate origin audit must not initialize an absent workspace or attach
/// an event to a memory in a different workspace merely because its ID exists.
fn audit_demotion_origin(
    options: &DemoteGlobalOptions<'_>,
    memory: &crate::db::StoredMemory,
) -> Result<(), String> {
    let Some((workspace, id)) = memory
        .provenance_uri
        .as_deref()
        .and_then(parse_promotion_provenance)
    else {
        return Ok(());
    };
    let error = || "Could not record the demotion origin audit".to_owned();
    if workspace.parse::<crate::models::WorkspaceId>().is_err()
        || id.parse::<crate::models::MemoryId>().is_err()
        || !options
            .workspace_database_path
            .try_exists()
            .map_err(|_| error())?
    {
        return Err(error());
    }
    let source = DbConnection::open_file(options.workspace_database_path).map_err(|_| error())?;
    source
        .with_transaction(|| {
            global_mutation_target(&source, &workspace, &id)?;
            source.insert_audit(
                &generate_audit_id(),
                &CreateAuditInput {
                    workspace_id: Some(workspace.clone()),
                    actor: options.actor.map(str::to_owned),
                    action: "memory.demote_global".to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some(id.clone()),
                    details: Some(demotion_audit_details(memory)),
                },
            )?;
            Ok(())
        })
        .map_err(|_| error())
}

/// Demote (tombstone) a global row. The origin workspace row is never
/// touched — demotion withdraws the global copy, it does not delete
/// knowledge. Destination state is atomic; origin audit is a separate step.
///
/// # Errors
///
/// Storage failures before destination commit roll back the withdrawal. An
/// origin-audit failure explicitly reports the already-committed withdrawal;
/// retrying repairs the audit and index without rewriting the tombstone.
pub fn demote_global(options: &DemoteGlobalOptions<'_>) -> Result<DemotionReport, String> {
    let (global_connection, global_workspace_id) =
        admission::open_existing_global(options.global_paths, options.dry_run)?;
    if options.dry_run {
        let snapshot = admission::ReadSnapshot::begin(&global_connection)
            .map_err(|_| "Could not begin global demotion preview".to_owned())?;
        let row = global_mutation_target(
            &global_connection,
            &global_workspace_id,
            options.global_memory_id,
        )
        .map_err(|_| "Could not verify global demotion target".to_owned())?;
        let origin = row
            .provenance_uri
            .as_deref()
            .and_then(parse_promotion_provenance);
        snapshot
            .finish()
            .map_err(|_| "Could not release global demotion preview".to_owned())?;
        return Ok(DemotionReport {
            global_memory_id: row.id,
            executed: false,
            tombstoned: false,
            origin,
            index_job_id: None,
            index_status: "not_applicable".to_owned(),
            index_error: None,
        });
    }

    let (row, tombstoned, index_job_id) = persist_global_demotion(
        &global_connection,
        &global_workspace_id,
        options.global_memory_id,
        options.actor,
    )
    .map_err(|_| {
        "Global demotion transaction failed; inspect the destination before retrying".to_owned()
    })?;
    let origin = row
        .provenance_uri
        .as_deref()
        .and_then(parse_promotion_provenance);
    let origin_audit = audit_demotion_origin(options, &row);
    // Withdrawal must reach retrieval even when its separate origin audit is
    // unavailable. The durable queue also survives a crash or index failure.
    let index_report = super::memory::reconcile_committed_memory_index_job(
        &global_connection,
        &global_workspace_id,
        &index_job_id,
        &options.global_paths.index_dir,
    );
    let provisional_index_status = super::memory::remember_index_status(&index_report);
    let index_status = super::memory::authoritative_remember_index_status(
        &global_workspace_id,
        &options.global_paths.root,
        &options.global_paths.database_path,
        &options.global_paths.index_dir,
        std::slice::from_ref(&index_job_id),
        &provisional_index_status,
    );
    let index_error = index_report.error;
    let _ = global_connection.close();
    origin_audit.map_err(|_| format!(
        "global_demotion_origin_audit_pending: withdrawal of global memory {} is committed and index repair is queued; retry demotion with the origin workspace to repair its audit",
        row.id
    ))?;

    Ok(DemotionReport {
        global_memory_id: row.id,
        executed: true,
        tombstoned,
        origin,
        index_job_id: Some(index_job_id),
        index_status,
        index_error,
    })
}

// ── Feedback backflow (slice 3) ────────────────────────────────────────────

pub const GLOBAL_BACKFLOW_REPORT_SCHEMA_V1: &str = "ee.global_promotion.backflow.v1";

/// Hard cap on how much one global-lane outcome may move the origin row's
/// confidence. Backflow is corroboration, not authority: a run of global
/// outcomes adjusts the origin gradually and each step is audited.
pub const MAX_BACKFLOW_STEP: f32 = 0.05;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackflowSignal {
    Helpful,
    Harmful,
}

impl BackflowSignal {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Helpful => "helpful",
            Self::Harmful => "harmful",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BackflowOptions<'a> {
    pub workspace_database_path: &'a Path,
    pub global_memory_id: &'a str,
    pub global_paths: &'a super::global_store::GlobalStorePaths,
    pub signal: BackflowSignal,
    /// Requested magnitude; clamped to [`MAX_BACKFLOW_STEP`].
    pub weight: f32,
    pub actor: Option<&'a str>,
    pub dry_run: bool,
}

#[derive(Clone, Debug)]
pub struct BackflowReport {
    pub global_memory_id: String,
    /// `None` when the global row carries no promotion provenance (it was
    /// written directly with `remember --global`) — feedback is recorded on
    /// the global row but there is no origin to adjust.
    pub origin: Option<(String, String)>,
    pub applied_delta: f32,
    pub origin_confidence_before: Option<f32>,
    pub origin_confidence_after: Option<f32>,
    pub executed: bool,
}

impl BackflowReport {
    #[must_use]
    pub fn data_json(&self) -> Value {
        json!({
            "schema": GLOBAL_BACKFLOW_REPORT_SCHEMA_V1,
            "globalMemoryId": self.global_memory_id,
            "originWorkspaceId": self.origin.as_ref().map(|(workspace, _)| workspace.clone()),
            "originMemoryId": self.origin.as_ref().map(|(_, memory)| memory.clone()),
            "appliedDelta": self.applied_delta,
            "originConfidenceBefore": self.origin_confidence_before,
            "originConfidenceAfter": self.origin_confidence_after,
            "executed": self.executed,
        })
    }
}

/// There is no historical backflow mode: only a current, revealed revision
/// within its authored validity window may drive a confidence adjustment.
/// The caller owns the snapshot/transaction covering the body and sidecars.
fn backflow_target_is_current(
    connection: &DbConnection,
    memory: &crate::db::StoredMemory,
    reference: chrono::DateTime<chrono::Utc>,
) -> crate::db::Result<bool> {
    let parse = |raw: &str| {
        chrono::DateTime::parse_from_rfc3339(raw)
            .map(|time| time.with_timezone(&chrono::Utc))
            .map_err(|_| global_mutation_error("Invalid feedback lifecycle metadata"))
    };
    let from = memory.valid_from.as_deref().map(parse).transpose()?;
    let to = memory.valid_to.as_deref().map(parse).transpose()?;
    if from.zip(to).is_some_and(|(from, to)| from > to) {
        return Err(global_mutation_error("Invalid feedback validity window"));
    }
    let rows = connection.query(
        "SELECT superseded_at FROM memories WHERE id = ?1 AND workspace_id = ?2",
        &[
            sqlmodel_core::Value::Text(memory.id.clone()),
            sqlmodel_core::Value::Text(memory.workspace_id.clone()),
        ],
    )?;
    let row = rows
        .first()
        .filter(|_| rows.len() == 1)
        .ok_or_else(|| global_mutation_error("Feedback source identity changed"))?;
    let superseded = match row.get(0) {
        Some(sqlmodel_core::Value::Null) => false,
        Some(sqlmodel_core::Value::Text(raw)) => {
            parse(raw)?;
            true
        }
        _ => return Err(global_mutation_error("Invalid feedback revision metadata")),
    };
    let seal = connection.get_memory_seal(&memory.id)?;
    if let Some(raw) = seal.as_ref().and_then(|seal| seal.revealed_at.as_deref()) {
        parse(raw)?;
    }
    Ok(memory.tombstoned_at.is_none()
        && !superseded
        && from.is_none_or(|from| from <= reference)
        && to.is_none_or(|to| reference <= to)
        && seal.is_none_or(|seal| !seal.is_sealed()))
}

fn verified_backflow_origin(
    memory: &crate::db::StoredMemory,
) -> crate::db::Result<Option<(String, String)>> {
    let Some(uri) = memory
        .provenance_uri
        .as_deref()
        .filter(|uri| uri.starts_with("ee-mem://"))
    else {
        return Ok(None);
    };
    let (workspace, id) = parse_promotion_provenance(uri)
        .ok_or_else(|| global_mutation_error("Invalid feedback origin provenance"))?;
    if workspace.parse::<crate::models::WorkspaceId>().is_err()
        || id.parse::<crate::models::MemoryId>().is_err()
    {
        return Err(global_mutation_error("Invalid feedback origin identity"));
    }
    Ok(Some((workspace, id)))
}

/// Round toward the starting confidence when nearest-f32 rounding would
/// exceed the requested step. Both endpoints are finite nonnegative unit
/// scores, so adjacent positive float bit patterns are ordered numerically.
fn bounded_backflow_target(before: f32, requested: f32) -> f32 {
    let after = (before + requested).clamp(0.0, 1.0);
    if (f64::from(after) - f64::from(before)).abs() > f64::from(requested.abs()) {
        if after > before {
            f32::from_bits(after.to_bits() - 1)
        } else {
            f32::from_bits(after.to_bits() + 1)
        }
    } else {
        after
    }
}

/// Read current confidence and commit its change, index repair, and audit as
/// one unit. A concurrent writer either serializes or causes a reported
/// transaction failure, never a successful lost update. Retired/missing or
/// in-place-reworded origins retain historical feedback only in the global DB.
fn persist_origin_backflow(
    connection: &DbConnection,
    options: &BackflowOptions<'_>,
    global: &crate::db::StoredMemory,
    origin: &(String, String),
    feedback_id: &str,
    reference: chrono::DateTime<chrono::Utc>,
) -> crate::db::Result<Option<(f32, f32)>> {
    if !options.weight.is_finite() {
        return Err(global_mutation_error("Feedback weight must be finite"));
    }
    connection.with_transaction(|| {
        let (workspace, id) = origin;
        let Some(memory) = connection.get_memory(id)? else {
            return Ok(None);
        };
        if memory.workspace_id != *workspace {
            return Err(global_mutation_error("Feedback origin workspace mismatch"));
        }
        if !backflow_target_is_current(connection, &memory, reference)?
            || memory.content != global.content
        {
            return Ok(None);
        }
        let before = memory.confidence;
        if !before.is_finite() || !(0.0..=1.0).contains(&before) {
            return Err(global_mutation_error("Invalid origin confidence"));
        }
        let step = options.weight.clamp(0.0, MAX_BACKFLOW_STEP);
        let requested = match options.signal {
            BackflowSignal::Helpful => step,
            BackflowSignal::Harmful => -step,
        };
        let after = bounded_backflow_target(before, requested);
        if before == after {
            return Ok(Some((before, after)));
        }
        if !connection.apply_memory_reinforcement(id, workspace, after, &reference.to_rfc3339())? {
            return Err(global_mutation_error("Origin feedback was not applied"));
        }
        // Confidence contributes to retrieval metadata. Do not leave a
        // successful learning update behind an apparently current index.
        let job = promotion_index_job_id();
        connection.insert_search_index_job(
            &job,
            &CreateSearchIndexJobInput {
                workspace_id: workspace.clone(),
                job_type: SearchIndexJobType::SingleDocument,
                document_source: Some("memory".to_owned()),
                document_id: Some(id.clone()),
                documents_total: 1,
            },
        )?;
        connection.insert_audit(
            &generate_audit_id(),
            &CreateAuditInput {
                workspace_id: Some(workspace.clone()),
                actor: options.actor.map(str::to_owned),
                action: "memory.global_feedback_backflow".to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some(id.clone()),
                details: Some(
                    json!({
                        "schema": GLOBAL_BACKFLOW_REPORT_SCHEMA_V1,
                        "globalMemoryId": global.id,
                        "feedbackEventId": feedback_id,
                        "signal": options.signal.as_str(),
                        "requestedDelta": requested,
                        "appliedDelta": after - before,
                        "confidenceBefore": before,
                        "confidenceAfter": after,
                        "indexJobId": job,
                    })
                    .to_string(),
                ),
            },
        )?;
        Ok(Some((before, after)))
    })
}

/// Record global outcome evidence and apply a bounded origin adjustment.
/// Executed reports state the actual origin delta, including zero when no
/// current origin can be adjusted. Previews retain the requested-delta form
/// and never open the origin store. Global and origin commits are separate.
///
/// # Errors
///
/// A failure after recording global feedback names the committed event and
/// explicitly withholds a claim of origin success. Do not blindly resubmit:
/// that would record another observation, not retry the same event.
pub fn backflow_global_feedback(options: &BackflowOptions<'_>) -> Result<BackflowReport, String> {
    if !options.weight.is_finite() {
        return Err("Global feedback weight must be finite".to_owned());
    }
    let (global_connection, global_workspace_id) =
        admission::open_existing_global(options.global_paths, options.dry_run)?;
    let reference = chrono::Utc::now();
    let step = options.weight.clamp(0.0, MAX_BACKFLOW_STEP);
    let requested = match options.signal {
        BackflowSignal::Helpful => step,
        BackflowSignal::Harmful => -step,
    };
    if options.dry_run {
        let snapshot = admission::ReadSnapshot::begin(&global_connection)
            .map_err(|_| "Could not begin global feedback preview".to_owned())?;
        let row = global_mutation_target(
            &global_connection,
            &global_workspace_id,
            options.global_memory_id,
        )
        .map_err(|_| "Could not verify global feedback target".to_owned())?;
        let origin = verified_backflow_origin(&row)
            .map_err(|_| "Could not verify global feedback origin".to_owned())?;
        snapshot
            .finish()
            .map_err(|_| "Could not release global feedback preview".to_owned())?;
        return Ok(BackflowReport {
            global_memory_id: row.id,
            origin,
            applied_delta: requested,
            origin_confidence_before: None,
            origin_confidence_after: None,
            executed: false,
        });
    }

    let feedback_id = promotion_feedback_event_id();
    let (row, origin, current) =
        global_connection
            .with_transaction(|| {
                let row = global_mutation_target(
                    &global_connection,
                    &global_workspace_id,
                    options.global_memory_id,
                )?;
                let origin = verified_backflow_origin(&row)?;
                let current = backflow_target_is_current(&global_connection, &row, reference)?;
                // Feedback about retired knowledge remains useful historical evidence;
                // it must not silently alter an otherwise current origin memory.
                global_connection.insert_feedback_event(
            &feedback_id,
            &crate::db::CreateFeedbackEventInput {
                workspace_id: global_workspace_id.clone(),
                target_type: "memory".to_owned(),
                target_id: row.id.clone(),
                signal: options.signal.as_str().to_owned(),
                weight: step,
                source_type: "outcome_observed".to_owned(),
                source_id: options.actor.map(str::to_owned),
                reason: Some("global-lane outcome evidence (backflow)".to_owned()),
                evidence_json: Some(json!({
                    "schema": GLOBAL_BACKFLOW_REPORT_SCHEMA_V1,
                    "originWorkspaceId": origin.as_ref().map(|(workspace, _)| workspace),
                    "originMemoryId": origin.as_ref().map(|(_, id)| id),
                    "requestedDelta": requested,
                    "sourceCurrent": current,
                }).to_string()),
                session_id: None,
            },
        )?;
                Ok((row, origin, current))
            })
            .map_err(|_| {
                "Could not commit global feedback; inspect the global store before retrying"
                    .to_owned()
            })?;

    let adjustment = if current && let Some(origin) = &origin {
        let result = (|| -> Result<Option<(f32, f32)>, String> {
            if !options
                .workspace_database_path
                .try_exists()
                .map_err(|_| "Could not inspect origin store".to_owned())?
            {
                return Err("Origin store does not exist".to_owned());
            }
            let source = DbConnection::open_file(options.workspace_database_path)
                .map_err(|_| "Could not open existing origin store".to_owned())?;
            persist_origin_backflow(&source, options, &row, origin, &feedback_id, reference)
                .map_err(|_| "Could not commit origin backflow".to_owned())
        })();
        result.map_err(|_| format!(
            "global_feedback_origin_pending: feedback {feedback_id} is committed; the origin update was not confirmed; inspect both stores before retrying and do not blindly record the observation again"
        ))?
    } else {
        None
    };
    let (before, after, applied_delta) = adjustment.map_or((None, None, 0.0), |(before, after)| {
        (Some(before), Some(after), after - before)
    });
    Ok(BackflowReport {
        global_memory_id: row.id,
        origin,
        applied_delta,
        origin_confidence_before: before,
        origin_confidence_after: after,
        executed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CreateWorkspaceInput;

    fn candidate(trust_class: &str) -> PromotionCandidate {
        PromotionCandidate {
            memory_id: "mem_00000000000000000000000001".to_owned(),
            workspace_id: "wsp_01234567890123456789012345".to_owned(),
            content: "Run cargo fmt --check before every release.".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            trust_class: trust_class.to_owned(),
            confidence: 0.9,
            tombstoned: false,
            sealed: false,
        }
    }

    fn input(candidate: PromotionCandidate) -> PromotionInput {
        PromotionInput {
            candidate,
            nearest_global_duplicate: None,
            merge_similarity: None,
            global_lane_available: true,
        }
    }

    #[test]
    fn validated_memory_promotes_as_insert() {
        let plan = plan_promotion(&input(candidate("agent_validated")));
        assert!(plan.allowed());
        assert!(matches!(
            plan.verdict,
            PromotionVerdict::Allow {
                action: PromotionAction::Insert
            }
        ));
        assert_eq!(plan.audit_action, "memory.promote_global");
    }

    #[test]
    fn evidence_gate_refuses_weak_trust_classes() {
        for trust in ["agent_assertion", "cass_evidence", "legacy_import"] {
            let plan = plan_promotion(&input(candidate(trust)));
            assert!(!plan.allowed(), "trust `{trust}` must be refused");
            let PromotionVerdict::Refuse { refusal } = &plan.verdict else {
                panic!("expected refusal for {trust}");
            };
            assert_eq!(refusal.code(), "global_promotion_evidence_gate");
            assert!(refusal.message().contains(trust));
        }
        // Both strong classes pass.
        for trust in ["human_explicit", "agent_validated"] {
            assert!(plan_promotion(&input(candidate(trust))).allowed());
        }
    }

    #[test]
    fn secret_like_content_refuses_with_stable_code() {
        let mut secret = candidate("human_explicit");
        secret.content =
            "Deploy key: AKIAIOSFODNN7EXAMPLE and token ghp_0123456789abcdefghijklmnopqrstuvwxyz"
                .to_owned();
        let plan = plan_promotion(&input(secret));
        let PromotionVerdict::Refuse { refusal } = &plan.verdict else {
            panic!("secret content must refuse");
        };
        assert_eq!(refusal.code(), GLOBAL_PROMOTION_REDACTION_REFUSED_CODE);
        assert!(refusal.message().contains("refuses rather than silently"));
        assert_eq!(plan.audit_action, "memory.promote_global_refused");
    }

    #[test]
    fn tombstoned_sealed_and_lane_off_refuse_without_content_heuristics() {
        let mut dead = input(candidate("human_explicit"));
        dead.candidate.tombstoned = true;
        assert!(!plan_promotion(&dead).allowed());

        let mut sealed = input(candidate("human_explicit"));
        sealed.candidate.content = crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT.to_owned();
        sealed.candidate.sealed = true;
        let sealed_plan = plan_promotion(&sealed);
        let PromotionVerdict::Refuse { refusal } = &sealed_plan.verdict else {
            panic!("sealed=true must refuse");
        };
        assert_eq!(refusal.code(), "global_promotion_sealed");
        assert!(refusal.repair().contains("ee memory reveal"));

        let mut identical_unsealed = input(candidate("human_explicit"));
        identical_unsealed.candidate.content =
            crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT.to_owned();
        identical_unsealed.candidate.sealed = false;
        assert!(
            plan_promotion(&identical_unsealed).allowed(),
            "identical public content with sealed=false must not be refused"
        );

        let mut off = input(candidate("human_explicit"));
        off.global_lane_available = false;
        assert!(!plan_promotion(&off).allowed());
    }

    #[test]
    fn near_duplicate_merges_instead_of_inserting() {
        let mut merging = input(candidate("agent_validated"));
        merging.nearest_global_duplicate = Some(GlobalNearDuplicate {
            global_memory_id: "mem_g0000000000000000000000001".to_owned(),
            similarity: 0.95,
        });
        let plan = plan_promotion(&merging);
        assert!(matches!(
            &plan.verdict,
            PromotionVerdict::Allow {
                action: PromotionAction::MergeInto { global_memory_id }
            } if global_memory_id == "mem_g0000000000000000000000001"
        ));

        // Below the threshold: plain insert.
        let mut distinct = input(candidate("agent_validated"));
        distinct.nearest_global_duplicate = Some(GlobalNearDuplicate {
            global_memory_id: "mem_g0000000000000000000000001".to_owned(),
            similarity: 0.5,
        });
        assert!(matches!(
            plan_promotion(&distinct).verdict,
            PromotionVerdict::Allow {
                action: PromotionAction::Insert
            }
        ));
    }

    fn seeded_workspace(
        temp: &Path,
        trust_class: &str,
        content: &str,
    ) -> (std::path::PathBuf, String) {
        std::fs::create_dir_all(temp).expect("create workspace dir");
        let database_path = temp.join("workspace.db");
        let connection = DbConnection::open_file(&database_path).expect("open workspace db");
        connection.migrate().expect("migrate workspace db");
        connection
            .execute_raw(
                "INSERT INTO workspaces (id, path, created_at, updated_at) VALUES ('wsp_01234567890123456789012345', '/tmp/promo-ws', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            )
            .expect("seed workspace");
        let memory_id = crate::models::MemoryId::now().to_string();
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id: "wsp_01234567890123456789012345".to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: content.to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: trust_class.to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("seed memory");
        connection.close().expect("close workspace db");
        (database_path, memory_id)
    }

    #[test]
    fn promote_global_seal_lookup_failure_refuses_before_admission() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (workspace_db, memory_id) = seeded_workspace(
            temp.path(),
            "human_explicit",
            crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT,
        );
        let connection = DbConnection::open_file(&workspace_db).expect("open workspace db");
        connection
            .execute_raw("DROP TABLE memory_seals")
            .expect("remove sidecar table for planted failure");
        connection.close().expect("close workspace db");
        let paths =
            super::super::global_store::GlobalStorePaths::from_root(&temp.path().join("global"));

        let error = promote_global(&PromoteGlobalOptions {
            workspace_database_path: &workspace_db,
            memory_id: &memory_id,
            global_paths: &paths,
            global_lane_available: true,
            actor: None,
            dry_run: false,
        })
        .expect_err("seal sidecar lookup failure must not admit the promotion candidate");
        assert!(
            error.contains("verify memory seal sidecar"),
            "lookup refusal must name the failed truth source: {error}"
        );
    }

    #[test]
    fn promote_inserts_audits_both_stores_and_repromotes_idempotently() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (workspace_db, memory_id) = seeded_workspace(
            temp.path(),
            "agent_validated",
            "Always pin franken-stack revisions before remote verification.",
        );
        let paths =
            super::super::global_store::GlobalStorePaths::from_root(&temp.path().join("global"));

        let options = PromoteGlobalOptions {
            workspace_database_path: &workspace_db,
            memory_id: &memory_id,
            global_paths: &paths,
            global_lane_available: true,
            actor: Some("test-actor"),
            dry_run: false,
        };
        let report = promote_global(&options).expect("promotion");
        assert!(report.executed);
        assert!(!report.already_promoted);
        assert!(report.index_job_id.is_some());
        assert_eq!(report.index_status, "indexed");
        assert!(report.index_error.is_none());
        let global_id = report.global_memory_id.clone().expect("global id");

        // The global row exists, carries origin provenance and trust.
        let (global_connection, global_ws) =
            super::super::global_store::open_or_create_global_store(&paths).expect("open global");
        let row = global_connection
            .get_memory(&global_id)
            .expect("load")
            .expect("global row");
        assert_eq!(row.trust_class, "agent_validated");
        assert_eq!(
            row.provenance_uri.as_deref(),
            Some(promotion_provenance_uri("wsp_01234567890123456789012345", &memory_id).as_str())
        );
        assert_eq!(row.workspace_id, global_ws);
        let _ = global_connection.close();

        // Re-promotion is an idempotent merge, not a twin insert.
        let again = promote_global(&options).expect("re-promotion");
        assert!(again.already_promoted);
        assert_eq!(again.global_memory_id.as_deref(), Some(global_id.as_str()));
        assert!(again.index_job_id.is_none());
        assert_eq!(again.index_status, "not_applicable");

        // Demotion tombstones the global row and parses origin back.
        let demotion = demote_global(&DemoteGlobalOptions {
            workspace_database_path: &workspace_db,
            global_memory_id: &global_id,
            global_paths: &paths,
            actor: Some("test-actor"),
            dry_run: false,
        })
        .expect("demotion");
        assert!(demotion.executed && demotion.tombstoned);
        assert!(demotion.index_job_id.is_some());
        assert_eq!(demotion.index_status, "indexed");
        assert!(demotion.index_error.is_none());
        assert_eq!(
            demotion.origin,
            Some((
                "wsp_01234567890123456789012345".to_owned(),
                memory_id.clone()
            ))
        );
    }

    #[test]
    fn refused_and_dry_run_promotions_write_nothing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (workspace_db, memory_id) =
            seeded_workspace(temp.path(), "agent_assertion", "Unvalidated hunch.");
        let paths =
            super::super::global_store::GlobalStorePaths::from_root(&temp.path().join("global"));

        let refused = promote_global(&PromoteGlobalOptions {
            workspace_database_path: &workspace_db,
            memory_id: &memory_id,
            global_paths: &paths,
            global_lane_available: true,
            actor: None,
            dry_run: false,
        })
        .expect("refusal is a report, not an error");
        assert!(!refused.executed);
        assert!(!refused.plan.allowed());

        // Dry-run of an allowed promotion also writes nothing.
        let (workspace_db2, memory_id2) = seeded_workspace(
            &temp.path().join("second"),
            "human_explicit",
            "Validated rule for dry-run.",
        );
        let dry = promote_global(&PromoteGlobalOptions {
            workspace_database_path: &workspace_db2,
            memory_id: &memory_id2,
            global_paths: &paths,
            global_lane_available: true,
            actor: None,
            dry_run: true,
        })
        .expect("dry-run");
        assert!(!dry.executed && dry.plan.allowed());

        let (global_connection, global_ws) =
            super::super::global_store::open_or_create_global_store(&paths).expect("open global");
        for content in ["Unvalidated hunch.", "Validated rule for dry-run."] {
            assert!(
                global_connection
                    .find_active_memory_by_content(&global_ws, content)
                    .expect("scan")
                    .is_none(),
                "nothing may be written for refused/dry-run promotions"
            );
        }
        let _ = global_connection.close();
    }

    #[test]
    fn backflow_adjusts_origin_bounded_and_audited() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (workspace_db, memory_id) = seeded_workspace(
            temp.path(),
            "agent_validated",
            "Backflow target rule with known confidence.",
        );
        let paths =
            super::super::global_store::GlobalStorePaths::from_root(&temp.path().join("global"));
        let promoted = promote_global(&PromoteGlobalOptions {
            workspace_database_path: &workspace_db,
            memory_id: &memory_id,
            global_paths: &paths,
            global_lane_available: true,
            actor: None,
            dry_run: false,
        })
        .expect("promotion");
        let global_id = promoted.global_memory_id.expect("global id");

        // Helpful outcome with an oversized weight: clamped to the step cap.
        let report = backflow_global_feedback(&BackflowOptions {
            workspace_database_path: &workspace_db,
            global_memory_id: &global_id,
            global_paths: &paths,
            signal: BackflowSignal::Helpful,
            weight: 0.5,
            actor: Some("test-actor"),
            dry_run: false,
        })
        .expect("backflow");
        assert!(report.executed);
        assert!((report.applied_delta - MAX_BACKFLOW_STEP).abs() < f32::EPSILON);
        let before = report.origin_confidence_before.expect("before");
        let after = report.origin_confidence_after.expect("after");
        assert!((after - (before + MAX_BACKFLOW_STEP)).abs() < 1e-6);

        // The origin row actually moved.
        let workspace_connection = DbConnection::open_file(&workspace_db).expect("open ws");
        let origin_row = workspace_connection
            .get_memory(&memory_id)
            .expect("load")
            .expect("row");
        assert!((origin_row.confidence - after).abs() < 1e-6);
        let _ = workspace_connection.close();

        // Harmful outcome moves it back down.
        let harmful = backflow_global_feedback(&BackflowOptions {
            workspace_database_path: &workspace_db,
            global_memory_id: &global_id,
            global_paths: &paths,
            signal: BackflowSignal::Harmful,
            weight: 0.02,
            actor: None,
            dry_run: false,
        })
        .expect("harmful backflow");
        assert!(harmful.applied_delta < 0.0);
        assert!(
            harmful.origin_confidence_after.expect("after") < after,
            "harmful signal must lower origin confidence"
        );

        // Dry run reports the would-be delta without touching anything.
        let dry = backflow_global_feedback(&BackflowOptions {
            workspace_database_path: &workspace_db,
            global_memory_id: &global_id,
            global_paths: &paths,
            signal: BackflowSignal::Helpful,
            weight: 0.01,
            actor: None,
            dry_run: true,
        })
        .expect("dry backflow");
        assert!(!dry.executed);
        assert!(dry.origin_confidence_after.is_none());
    }

    #[test]
    fn promotion_provenance_round_trips() {
        let uri = promotion_provenance_uri("wsp_a", "mem_b");
        assert_eq!(
            parse_promotion_provenance(&uri),
            Some(("wsp_a".to_owned(), "mem_b".to_owned()))
        );
        assert_eq!(parse_promotion_provenance("https://x/y"), None);
        assert_eq!(parse_promotion_provenance("ee-mem://only"), None);
    }

    #[test]
    fn plan_json_is_stable_and_actionable() {
        let plan = plan_promotion(&input(candidate("agent_validated")));
        let value = plan.data_json();
        assert_eq!(value["schema"], GLOBAL_PROMOTION_PLAN_SCHEMA_V1);
        assert_eq!(value["verdict"], "allow");
        assert_eq!(value["detail"]["action"], "insert");

        let refused = plan_promotion(&input(candidate("agent_assertion")));
        let value = refused.data_json();
        assert_eq!(value["verdict"], "refuse");
        assert!(
            value["detail"]["repair"]
                .as_str()
                .is_some_and(|repair| !repair.is_empty()),
            "refusals must carry an actionable repair"
        );
    }
    struct PublicationFixture {
        _temp: tempfile::TempDir,
        source_path: std::path::PathBuf,
        memory: crate::db::StoredMemory,
        paths: super::super::global_store::GlobalStorePaths,
        destination: DbConnection,
        workspace: String,
    }

    impl PublicationFixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("publication fixture");
            let (source_path, id) =
                seeded_workspace(temp.path(), "human_explicit", "Publication advice.");
            let source = DbConnection::open_file(&source_path).expect("source");
            source
                .execute_raw(&format!(
                    "UPDATE memories SET valid_from = '2020-01-01T00:00:00Z', valid_to = '2099-01-01T00:00:00Z' WHERE id = '{id}'"
                ))
                .expect("authored lifetime");
            let memory = source
                .get_memory(&id)
                .expect("source read")
                .expect("source row");
            source.close().expect("close source");
            let paths = super::super::global_store::GlobalStorePaths::from_root(
                &temp.path().join("global"),
            );
            let (destination, workspace) =
                super::super::global_store::open_or_create_global_store(&paths)
                    .expect("destination");
            Self {
                _temp: temp,
                source_path,
                memory,
                paths,
                destination,
                workspace,
            }
        }

        fn publish(&self) -> crate::db::Result<(String, bool, Option<String>)> {
            persist_global_promotion(
                &self.destination,
                &self.workspace,
                &self.memory,
                &PromotionPayload::default(),
                Some("test-actor"),
                chrono::Utc::now(),
            )
        }

        fn count(&self, table: &str) -> i64 {
            self.destination
                .count_table_rows(table)
                .expect("durable count")
        }

        fn options(&self, dry_run: bool) -> PromoteGlobalOptions<'_> {
            PromoteGlobalOptions {
                workspace_database_path: &self.source_path,
                memory_id: &self.memory.id,
                global_paths: &self.paths,
                global_lane_available: true,
                actor: Some("publication-retry"),
                dry_run,
            }
        }
    }

    #[test]
    fn global_publication_preserves_lifetime_and_commits_one_repairable_job() {
        let f = PublicationFixture::new();
        let before = (
            f.count("memories"),
            f.count("search_index_jobs"),
            f.count("audit_log"),
        );
        let (id, already, job) = f.publish().expect("commit");
        assert!(!already && job.is_some());
        let copy = f
            .destination
            .get_memory(&id)
            .expect("read")
            .expect("global copy");
        assert_eq!(copy.valid_from, f.memory.valid_from);
        assert_eq!(copy.valid_to, f.memory.valid_to);
        assert_eq!(copy.content, f.memory.content);
        assert_eq!(copy.trust_class, f.memory.trust_class);
        assert_eq!(
            copy.provenance_uri,
            Some(promotion_provenance_uri(
                &f.memory.workspace_id,
                &f.memory.id
            ))
        );
        assert_eq!(
            (
                f.count("memories"),
                f.count("search_index_jobs"),
                f.count("audit_log"),
            ),
            (before.0 + 1, before.1 + 1, before.2 + 1)
        );
        let rows = f
            .destination
            .query(
                "SELECT document_id FROM search_index_jobs WHERE id = ?1",
                &[sqlmodel_core::Value::Text(job.clone().expect("job id"))],
            )
            .expect("durable repair job");
        assert!(matches!(
            rows[0].get(0),
            Some(sqlmodel_core::Value::Text(target)) if target == &id
        ));
        let (again, already, retry_job) = f.publish().expect("idempotent retry");
        assert_eq!(again, id);
        assert!(already);
        assert_eq!(retry_job, job, "retry must retain the unpublished receipt");
        assert_eq!(f.count("memories"), before.0 + 1);
        assert_eq!(f.count("search_index_jobs"), before.1 + 1);
    }

    #[test]
    fn missing_index_queue_rolls_back_the_memory_and_its_tags() {
        let f = PublicationFixture::new();
        let before = (
            f.count("memories"),
            f.count("memory_tags"),
            f.count("audit_log"),
        );
        f.destination
            .execute_raw("ALTER TABLE search_index_jobs RENAME TO unavailable_promotion_jobs")
            .expect("plant queue failure");
        assert!(f.publish().is_err());
        assert_eq!(
            (
                f.count("memories"),
                f.count("memory_tags"),
                f.count("audit_log"),
            ),
            before
        );
        f.destination
            .execute_raw("ALTER TABLE unavailable_promotion_jobs RENAME TO search_index_jobs")
            .expect("repair queue");
        assert!(f.publish().expect("retry after rollback").2.is_some());
    }

    #[test]
    fn missing_destination_audit_rolls_back_memory_tags_and_index_work() {
        let f = PublicationFixture::new();
        let before = (
            f.count("memories"),
            f.count("memory_tags"),
            f.count("search_index_jobs"),
        );
        f.destination
            .execute_raw("ALTER TABLE audit_log RENAME TO unavailable_promotion_audit")
            .expect("plant last-step failure");
        assert!(f.publish().is_err());
        assert_eq!(
            (
                f.count("memories"),
                f.count("memory_tags"),
                f.count("search_index_jobs"),
            ),
            before
        );
        f.destination
            .execute_raw("ALTER TABLE unavailable_promotion_audit RENAME TO audit_log")
            .expect("repair audit");
        let (id, already, job) = f.publish().expect("retry after rollback");
        assert!(!already && job.is_some());
        assert!(
            f.destination
                .get_memory(&id)
                .expect("read committed retry")
                .is_some()
        );
    }

    #[test]
    fn origin_audit_failure_reports_committed_destination_and_retry_does_not_duplicate() {
        let f = PublicationFixture::new();
        let source = DbConnection::open_file(&f.source_path).expect("source");
        source
            .execute_raw("ALTER TABLE audit_log RENAME TO unavailable_origin_audit")
            .expect("plant origin failure");
        source.close().expect("release schema-fixture connection");
        let options = PromoteGlobalOptions {
            workspace_database_path: &f.source_path,
            memory_id: &f.memory.id,
            global_paths: &f.paths,
            global_lane_available: true,
            actor: None,
            dry_run: false,
        };
        let error = promote_global(&options).expect_err("explicit committed-state error");
        assert!(error.contains("global_promotion_origin_audit_pending"));
        let copy = f
            .destination
            .find_active_memory_by_content(&f.workspace, &f.memory.content)
            .expect("read")
            .expect("committed destination");
        assert!(error.contains(&copy.id));
        assert_eq!(f.count("search_index_jobs"), 1);
        let jobs = f
            .destination
            .list_search_index_jobs(&f.workspace, None)
            .unwrap();
        assert_eq!(
            jobs[0].status_enum(),
            Some(crate::db::SearchIndexJobStatus::Completed),
            "origin-audit failure must not strand a committed destination's index job"
        );
        let source = DbConnection::open_file(&f.source_path).expect("fresh repair connection");
        source
            .execute_raw("ALTER TABLE unavailable_origin_audit RENAME TO audit_log")
            .expect("repair origin audit");
        let retry = promote_global(&options).expect("repair through re-promotion");
        assert!(retry.executed && retry.already_promoted);
        assert_eq!(retry.global_memory_id.as_deref(), Some(copy.id.as_str()));
        assert_eq!(f.count("memories"), 1);
        assert_eq!(f.count("search_index_jobs"), 1);
        assert_eq!(
            source
                .get_memory(&f.memory.id)
                .expect("source read")
                .expect("source row"),
            f.memory
        );
    }

    #[test]
    fn unbounded_authored_start_survives_publication_and_remains_idempotent() {
        let mut f = PublicationFixture::new();
        let source = DbConnection::open_file(&f.source_path).expect("source");
        source
            .execute_raw(&format!(
                "UPDATE memories SET valid_from = NULL WHERE id = '{}'",
                f.memory.id
            ))
            .expect("unbounded source");
        f.memory = source
            .get_memory(&f.memory.id)
            .expect("read")
            .expect("source row");
        let (id, already, first_job) = f.publish().expect("publish unbounded start");
        assert!(!already);
        let copy = f.destination.get_memory(&id).expect("read").expect("copy");
        assert_eq!(copy.valid_from, None);
        assert_eq!(copy.valid_to, f.memory.valid_to);
        let (again, already, job) = f.publish().expect("idempotent unbounded start");
        assert_eq!(again, id);
        assert!(already);
        assert_eq!(job, first_job, "retry keeps the same unpublished job");
    }

    fn set_retry_job_state(
        f: &PublicationFixture,
        job: &str,
        status: crate::db::SearchIndexJobStatus,
    ) {
        use crate::db::SearchIndexJobStatus;

        match status {
            SearchIndexJobStatus::Pending => {}
            SearchIndexJobStatus::Running => {
                assert!(f.destination.start_search_index_job(job).unwrap());
            }
            SearchIndexJobStatus::Failed => {
                assert!(f.destination.start_search_index_job(job).unwrap());
                assert!(
                    f.destination
                        .fail_search_index_job(job, "interrupted publication")
                        .unwrap()
                );
            }
            SearchIndexJobStatus::Cancelled => {
                assert!(f.destination.cancel_search_index_job(job).unwrap());
            }
            SearchIndexJobStatus::Completed => {
                assert!(f.destination.start_search_index_job(job).unwrap());
                assert!(f.destination.complete_search_index_job(job, 1).unwrap());
            }
        }
    }

    #[test]
    fn promotion_retry_retains_each_unfinished_receipt_without_stealing_its_state() {
        use crate::db::SearchIndexJobStatus;

        for status in [
            SearchIndexJobStatus::Pending,
            SearchIndexJobStatus::Running,
            SearchIndexJobStatus::Failed,
            SearchIndexJobStatus::Cancelled,
        ] {
            let f = PublicationFixture::new();
            let (id, _, job) = f.publish().unwrap();
            let job = job.unwrap();
            set_retry_job_state(&f, &job, status);
            let before = f.destination.get_search_index_job(&job).unwrap().unwrap();
            let copy = f.destination.get_memory(&id).unwrap().unwrap();
            let (again, already, receipt) = f.publish().unwrap();
            assert!(already);
            assert_eq!(again, id);
            assert_eq!(receipt.as_deref(), Some(job.as_str()));
            assert_eq!(
                f.destination.get_search_index_job(&job).unwrap().unwrap(),
                before
            );
            assert_eq!(f.destination.get_memory(&id).unwrap().unwrap(), copy);
            assert_eq!(f.count("search_index_jobs"), 1);
            assert_eq!(f.count("memories"), 1);
        }
    }

    #[test]
    fn promotion_retry_publication_recovers_the_original_receipt_after_interruption() {
        use crate::db::SearchIndexJobStatus;

        for status in [
            SearchIndexJobStatus::Pending,
            SearchIndexJobStatus::Running,
            SearchIndexJobStatus::Failed,
            SearchIndexJobStatus::Cancelled,
        ] {
            let f = PublicationFixture::new();
            // Model the crash boundary: durable memory/audit/job, no index.
            let (id, _, job) = f.publish().unwrap();
            let job = job.unwrap();
            set_retry_job_state(&f, &job, status);
            assert!(!f.paths.index_dir.exists());
            let report = promote_global(&f.options(false)).unwrap();
            assert!(report.executed && report.already_promoted);
            assert_eq!(report.global_memory_id.as_deref(), Some(id.as_str()));
            assert_eq!(report.index_job_id.as_deref(), Some(job.as_str()));
            assert_eq!(report.index_status, "indexed");
            assert!(report.index_error.is_none());
            let stored = f.destination.get_search_index_job(&job).unwrap().unwrap();
            assert_eq!(stored.status_enum(), Some(SearchIndexJobStatus::Completed));
            assert_eq!(stored.document_id.as_deref(), Some(id.as_str()));
            assert_eq!(f.count("memories"), 1);
            assert_eq!(f.count("search_index_jobs"), 1);
            let again = promote_global(&f.options(false)).unwrap();
            assert!(again.executed && again.already_promoted);
            assert!(again.index_job_id.is_none());
            assert_eq!(again.index_status, "not_applicable");
            assert_eq!(f.count("search_index_jobs"), 1);
        }
    }

    #[test]
    fn promotion_retry_preview_does_not_rearm_failed_publication_or_mutate_either_store() {
        let f = PublicationFixture::new();
        let (id, _, job) = f.publish().unwrap();
        let job = job.unwrap();
        set_retry_job_state(&f, &job, crate::db::SearchIndexJobStatus::Failed);
        let before = f.destination.get_search_index_job(&job).unwrap();
        let audits = f.count("audit_log");
        let source = DbConnection::open_file_read_only(&f.source_path).unwrap();
        let source_audits = source.count_table_rows("audit_log").unwrap();
        let report = promote_global(&f.options(true)).unwrap();
        assert!(!report.executed && report.already_promoted);
        assert_eq!(report.global_memory_id.as_deref(), Some(id.as_str()));
        assert!(report.index_job_id.is_none());
        assert_eq!(f.destination.get_search_index_job(&job).unwrap(), before);
        assert_eq!(f.count("audit_log"), audits);
        assert_eq!(source.count_table_rows("audit_log").unwrap(), source_audits);
        assert_eq!(source.get_memory(&f.memory.id).unwrap().unwrap(), f.memory);
        assert!(!f.paths.index_dir.exists());
    }

    #[test]
    fn promotion_retry_receipt_selection_is_entity_specific_and_deterministic() {
        let f = PublicationFixture::new();
        let (id, _, completed) = f.publish().unwrap();
        let completed = completed.unwrap();
        set_retry_job_state(&f, &completed, crate::db::SearchIndexJobStatus::Completed);
        let other = "wsp_00000000000000000000000098";
        f.destination
            .insert_workspace(
                other,
                &CreateWorkspaceInput {
                    path: f.paths.root.join("other").to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .unwrap();
        let mut valid = Vec::new();
        for (workspace, source, target, job_type) in [
            (
                other,
                "memory",
                id.as_str(),
                SearchIndexJobType::SingleDocument,
            ),
            (
                f.workspace.as_str(),
                "session",
                id.as_str(),
                SearchIndexJobType::SingleDocument,
            ),
            (
                f.workspace.as_str(),
                "memory",
                "mem_other",
                SearchIndexJobType::SingleDocument,
            ),
            (
                f.workspace.as_str(),
                "memory",
                id.as_str(),
                SearchIndexJobType::FullRebuild,
            ),
            (
                f.workspace.as_str(),
                "memory",
                id.as_str(),
                SearchIndexJobType::SingleDocument,
            ),
            (
                f.workspace.as_str(),
                "memory",
                id.as_str(),
                SearchIndexJobType::SingleDocument,
            ),
        ] {
            let job = promotion_index_job_id();
            f.destination
                .insert_search_index_job(
                    &job,
                    &CreateSearchIndexJobInput {
                        workspace_id: workspace.to_owned(),
                        job_type,
                        document_source: Some(source.to_owned()),
                        document_id: Some(target.to_owned()),
                        documents_total: 1,
                    },
                )
                .unwrap();
            if workspace == f.workspace.as_str()
                && source == "memory"
                && target == id.as_str()
                && job_type == SearchIndexJobType::SingleDocument
            {
                valid.push(job);
            }
        }
        // Equal clocks must be resolved by stable ID order, not row insertion.
        f.destination
            .execute_raw("UPDATE search_index_jobs SET created_at = '2026-09-17T00:00:00Z'")
            .unwrap();
        valid.sort();
        let before = f
            .destination
            .list_search_index_jobs(&f.workspace, None)
            .unwrap();
        assert_eq!(
            unfinished_promotion_index_job(&f.destination, &f.workspace, &id).unwrap(),
            Some(valid[0].clone())
        );
        assert!(
            unfinished_promotion_index_job(&f.destination, &f.workspace, "' OR 1 = 1 --")
                .unwrap()
                .is_none()
        );
        let (again, already, receipt) = f.publish().unwrap();
        assert!(already && again == id);
        assert_eq!(receipt, Some(valid[0].clone()));
        assert_eq!(
            f.destination
                .list_search_index_jobs(&f.workspace, None)
                .unwrap(),
            before
        );
    }

    #[test]
    fn promotion_retry_missing_receipt_storage_cannot_commit_a_false_duplicate_success() {
        let f = PublicationFixture::new();
        let (id, _, job) = f.publish().unwrap();
        let before = (f.count("audit_log"), f.count("memories"));
        f.destination
            .execute_raw("ALTER TABLE search_index_jobs RENAME TO unavailable_retry_jobs")
            .unwrap();
        assert!(f.publish().is_err());
        assert_eq!((f.count("audit_log"), f.count("memories")), before);
        f.destination
            .execute_raw("ALTER TABLE unavailable_retry_jobs RENAME TO search_index_jobs")
            .unwrap();
        let (again, already, receipt) = f.publish().unwrap();
        assert!(already && again == id);
        assert_eq!(receipt, job);
    }

    #[test]
    fn global_demotion_commits_tombstone_job_and_audit_without_changing_origin() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().expect("publish");
        let before = (f.count("search_index_jobs"), f.count("audit_log"));
        let (_, changed, job) =
            persist_global_demotion(&f.destination, &f.workspace, &id, Some("withdrawal-test"))
                .expect("withdraw");
        assert!(changed);
        let row = f.destination.get_memory(&id).unwrap().unwrap();
        assert!(row.tombstoned_at.is_some());
        assert_eq!(row.content, f.memory.content);
        assert_eq!(
            (f.count("search_index_jobs"), f.count("audit_log")),
            (before.0 + 1, before.1 + 1)
        );
        let jobs = f
            .destination
            .query(
                "SELECT document_id FROM search_index_jobs WHERE id = ?1",
                &[sqlmodel_core::Value::Text(job)],
            )
            .unwrap();
        assert!(
            matches!(jobs[0].get(0), Some(sqlmodel_core::Value::Text(target)) if target == &id)
        );
        let visible =
            super::super::global_store::read_global_store_memories(&f.paths, false).unwrap();
        assert!(visible.iter().all(|memory| memory.id != id));
        let source = DbConnection::open_file_read_only(&f.source_path).unwrap();
        assert_eq!(source.get_memory(&f.memory.id).unwrap().unwrap(), f.memory);
    }

    #[test]
    fn global_demotion_queue_failure_rolls_back_the_tombstone() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let before = f.destination.get_memory(&id).unwrap();
        let audits = f.count("audit_log");
        f.destination
            .execute_raw("ALTER TABLE search_index_jobs RENAME TO unavailable_demotion_jobs")
            .unwrap();
        assert!(persist_global_demotion(&f.destination, &f.workspace, &id, None).is_err());
        assert_eq!(f.destination.get_memory(&id).unwrap(), before);
        assert_eq!(f.count("audit_log"), audits);
        f.destination
            .execute_raw("ALTER TABLE unavailable_demotion_jobs RENAME TO search_index_jobs")
            .unwrap();
        assert!(
            persist_global_demotion(&f.destination, &f.workspace, &id, None)
                .unwrap()
                .1
        );
    }

    #[test]
    fn global_demotion_audit_failure_rolls_back_tombstone_and_index_job() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let before = f.destination.get_memory(&id).unwrap();
        let jobs = f.count("search_index_jobs");
        f.destination
            .execute_raw("ALTER TABLE audit_log RENAME TO unavailable_demotion_audit")
            .unwrap();
        assert!(persist_global_demotion(&f.destination, &f.workspace, &id, None).is_err());
        assert_eq!(f.destination.get_memory(&id).unwrap(), before);
        assert_eq!(f.count("search_index_jobs"), jobs);
        f.destination
            .execute_raw("ALTER TABLE unavailable_demotion_audit RENAME TO audit_log")
            .unwrap();
        assert!(
            persist_global_demotion(&f.destination, &f.workspace, &id, None)
                .unwrap()
                .1
        );
    }

    #[test]
    fn demotion_retries_preserve_original_tombstone_and_queue_fresh_index_repair() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let (_, changed, first_job) =
            persist_global_demotion(&f.destination, &f.workspace, &id, None).unwrap();
        assert!(changed);
        let first = f.destination.get_memory(&id).unwrap();
        let (_, changed, next_job) =
            persist_global_demotion(&f.destination, &f.workspace, &id, None).unwrap();
        assert!(!changed);
        assert_ne!(first_job, next_job);
        assert_eq!(f.destination.get_memory(&id).unwrap(), first);
    }

    #[test]
    fn demotion_rechecks_workspace_and_identity_before_any_mutation() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let before = f.destination.get_memory(&id).unwrap();
        let counts = (f.count("search_index_jobs"), f.count("audit_log"));
        assert!(
            persist_global_demotion(&f.destination, &f.memory.workspace_id, &id, None).is_err()
        );
        assert!(
            persist_global_demotion(&f.destination, &f.workspace, "PRIVATE_TARGET_CANARY", None)
                .is_err()
        );
        assert_eq!(f.destination.get_memory(&id).unwrap(), before);
        assert_eq!((f.count("search_index_jobs"), f.count("audit_log")), counts);
    }

    #[test]
    fn public_demotion_reports_committed_withdrawal_without_creating_missing_origin() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let missing = f._temp.path().join("missing-origin.db");
        let mut options = DemoteGlobalOptions {
            workspace_database_path: &missing,
            global_memory_id: &id,
            global_paths: &f.paths,
            actor: None,
            dry_run: false,
        };
        let error = demote_global(&options).expect_err("explicit partial outcome");
        assert!(error.contains("global_demotion_origin_audit_pending") && error.contains(&id));
        assert!(!missing.exists());
        let first = f.destination.get_memory(&id).unwrap();
        assert!(first.as_ref().unwrap().tombstoned_at.is_some());
        options.workspace_database_path = &f.source_path;
        let retry = demote_global(&options).expect("repair origin audit");
        assert!(retry.executed && !retry.tombstoned);
        assert_eq!(f.destination.get_memory(&id).unwrap(), first);
        let source = DbConnection::open_file_read_only(&f.source_path).unwrap();
        assert_eq!(source.get_memory(&f.memory.id).unwrap().unwrap(), f.memory);
        assert!(source.count_table_rows("audit_log").unwrap() > 0);
    }

    #[test]
    fn demotion_origin_audit_rejects_forged_workspace_attribution() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let mut row = f.destination.get_memory(&id).unwrap().unwrap();
        row.provenance_uri = Some(promotion_provenance_uri(
            "wsp_00000000000000000000000091",
            &f.memory.id,
        ));
        let source = DbConnection::open_file_read_only(&f.source_path).unwrap();
        let before = source.count_table_rows("audit_log").unwrap();
        assert!(
            audit_demotion_origin(
                &DemoteGlobalOptions {
                    workspace_database_path: &f.source_path,
                    global_memory_id: &id,
                    global_paths: &f.paths,
                    actor: None,
                    dry_run: false,
                },
                &row
            )
            .is_err()
        );
        assert_eq!(source.count_table_rows("audit_log").unwrap(), before);
        assert_eq!(source.get_memory(&f.memory.id).unwrap().unwrap(), f.memory);
    }

    fn backflow_options<'a>(f: &'a PublicationFixture, id: &'a str) -> BackflowOptions<'a> {
        BackflowOptions {
            workspace_database_path: &f.source_path,
            global_memory_id: id,
            global_paths: &f.paths,
            signal: BackflowSignal::Helpful,
            weight: 0.05,
            actor: Some("backflow-test"),
            dry_run: false,
        }
    }

    #[test]
    fn origin_backflow_commits_confidence_audit_and_index_repair_together() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let source = DbConnection::open_file(&f.source_path).unwrap();
        let before_jobs = source.count_table_rows("search_index_jobs").unwrap();
        let before_audits = source.count_table_rows("audit_log").unwrap();
        let report = backflow_global_feedback(&backflow_options(&f, &id)).unwrap();
        let memory = source.get_memory(&f.memory.id).unwrap().unwrap();
        assert_eq!(report.origin_confidence_before, Some(f.memory.confidence));
        assert_eq!(report.origin_confidence_after, Some(memory.confidence));
        assert_eq!(
            report.applied_delta,
            memory.confidence - f.memory.confidence
        );
        assert_eq!(memory.content, f.memory.content);
        assert_eq!(
            source.count_table_rows("search_index_jobs").unwrap(),
            before_jobs + 1
        );
        assert_eq!(
            source.count_table_rows("audit_log").unwrap(),
            before_audits + 1
        );
        let audit = source
            .query(
                "SELECT details FROM audit_log WHERE action = 'memory.global_feedback_backflow'",
                &[],
            )
            .unwrap();
        let Some(sqlmodel_core::Value::Text(details)) = audit[0].get(0) else {
            panic!("audit details");
        };
        let details: Value = serde_json::from_str(details).unwrap();
        assert_eq!(details["globalMemoryId"], id);
        assert!(
            details["feedbackEventId"]
                .as_str()
                .unwrap()
                .starts_with("fb_")
        );
        assert!(details["indexJobId"].as_str().unwrap().starts_with("sidx_"));
    }

    #[test]
    fn origin_backflow_audit_failure_rolls_back_confidence_and_index_repair() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let source = DbConnection::open_file(&f.source_path).unwrap();
        let before = source.get_memory(&f.memory.id).unwrap();
        let jobs = source.count_table_rows("search_index_jobs").unwrap();
        source
            .execute_raw("ALTER TABLE audit_log RENAME TO unavailable_backflow_audit")
            .unwrap();
        let error = backflow_global_feedback(&backflow_options(&f, &id)).unwrap_err();
        assert!(error.contains("global_feedback_origin_pending"));
        assert!(error.contains("feedback fb_") && error.contains("do not blindly"));
        assert!(!error.contains(&f.memory.content));
        assert_eq!(source.get_memory(&f.memory.id).unwrap(), before);
        assert_eq!(source.count_table_rows("search_index_jobs").unwrap(), jobs);
        assert_eq!(f.count("feedback_events"), 1);
    }

    #[test]
    fn origin_backflow_queue_failure_rolls_back_the_confidence_change() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let source = DbConnection::open_file(&f.source_path).unwrap();
        let before = source.get_memory(&f.memory.id).unwrap();
        let audits = source.count_table_rows("audit_log").unwrap();
        source
            .execute_raw("ALTER TABLE search_index_jobs RENAME TO unavailable_backflow_jobs")
            .unwrap();
        assert!(backflow_global_feedback(&backflow_options(&f, &id)).is_err());
        assert_eq!(source.get_memory(&f.memory.id).unwrap(), before);
        assert_eq!(source.count_table_rows("audit_log").unwrap(), audits);
        assert_eq!(f.count("feedback_events"), 1);
    }

    #[test]
    fn backflow_preserves_retired_future_expired_reworded_and_sealed_origins() {
        for update in [
            "tombstoned_at = '2021-01-01T00:00:00Z'",
            "superseded_at = '2099-01-01T00:00:00Z'",
            "valid_to = '2021-01-01T00:00:00Z'",
            "valid_from = '2098-01-01T00:00:00Z'",
            "content = 'Revised local advice.'",
            "confidence = confidence",
        ] {
            let f = PublicationFixture::new();
            let (id, _, _) = f.publish().unwrap();
            let source = DbConnection::open_file(&f.source_path).unwrap();
            source
                .execute_raw(&format!(
                    "UPDATE memories SET {update} WHERE id = '{}'",
                    f.memory.id
                ))
                .unwrap();
            if update == "confidence = confidence" {
                source
                    .insert_memory_seal(
                        &f.memory.id,
                        &crate::models::memory_seal_commitment(f.memory.content.as_bytes()),
                        "2020-01-01T00:00:00Z",
                    )
                    .unwrap();
            }
            let before = source.get_memory(&f.memory.id).unwrap();
            let counts = (
                source.count_table_rows("audit_log").unwrap(),
                source.count_table_rows("search_index_jobs").unwrap(),
            );
            let report = backflow_global_feedback(&backflow_options(&f, &id)).unwrap();
            assert!(report.executed && report.origin_confidence_after.is_none());
            assert_eq!(report.applied_delta, 0.0, "{update}");
            assert_eq!(source.get_memory(&f.memory.id).unwrap(), before);
            assert_eq!(
                (
                    source.count_table_rows("audit_log").unwrap(),
                    source.count_table_rows("search_index_jobs").unwrap()
                ),
                counts
            );
            assert_eq!(f.count("feedback_events"), 1);
        }
    }

    #[test]
    fn backflow_reports_actual_clamping_and_zero_when_no_origin_exists() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let source = DbConnection::open_file(&f.source_path).unwrap();
        source
            .execute_raw(&format!(
                "UPDATE memories SET confidence = 0.99 WHERE id = '{}'",
                f.memory.id
            ))
            .unwrap();
        let report = backflow_global_feedback(&backflow_options(&f, &id)).unwrap();
        assert_eq!(report.origin_confidence_after, Some(1.0));
        assert!((report.applied_delta - 0.01).abs() < 0.000001);
        let counts = (
            source.count_table_rows("audit_log").unwrap(),
            source.count_table_rows("search_index_jobs").unwrap(),
        );
        let saturated = backflow_global_feedback(&backflow_options(&f, &id)).unwrap();
        assert_eq!(saturated.applied_delta, 0.0);
        assert_eq!(saturated.origin_confidence_after, Some(1.0));
        assert_eq!(
            (
                source.count_table_rows("audit_log").unwrap(),
                source.count_table_rows("search_index_jobs").unwrap()
            ),
            counts
        );
        f.destination
            .execute_raw(&format!(
                "UPDATE memories SET provenance_uri = NULL WHERE id = '{id}'"
            ))
            .unwrap();
        let direct = backflow_global_feedback(&backflow_options(&f, &id)).unwrap();
        assert!(direct.origin.is_none() && direct.origin_confidence_after.is_none());
        assert_eq!(direct.applied_delta, 0.0);
    }

    #[test]
    fn feedback_on_withdrawn_global_knowledge_does_not_adjust_the_local_origin() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        persist_global_demotion(&f.destination, &f.workspace, &id, None).unwrap();
        let report = backflow_global_feedback(&backflow_options(&f, &id)).unwrap();
        assert!(report.executed && report.origin_confidence_after.is_none());
        assert_eq!(report.applied_delta, 0.0);
        assert_eq!(f.count("feedback_events"), 1);
        let source = DbConnection::open_file_read_only(&f.source_path).unwrap();
        assert_eq!(source.get_memory(&f.memory.id).unwrap().unwrap(), f.memory);
    }

    #[test]
    fn backflow_does_not_create_missing_origins_or_claim_their_update_succeeded() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let absent = f._temp.path().join("absent-feedback-origin.db");
        let mut options = backflow_options(&f, &id);
        options.workspace_database_path = &absent;
        let error = backflow_global_feedback(&options).unwrap_err();
        assert!(error.contains("global_feedback_origin_pending"));
        assert!(!absent.exists());
        assert_eq!(f.count("feedback_events"), 1);
    }

    #[test]
    fn global_feedback_preview_never_opens_the_origin_or_changes_either_store() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let absent = f._temp.path().join("absent-preview-origin.db");
        let source = DbConnection::open_file_read_only(&f.source_path).unwrap();
        let before = source.get_memory(&f.memory.id).unwrap();
        let mut options = backflow_options(&f, &id);
        options.workspace_database_path = &absent;
        options.dry_run = true;
        let report = backflow_global_feedback(&options).unwrap();
        assert!(!report.executed && report.origin_confidence_after.is_none());
        assert_eq!(report.applied_delta, MAX_BACKFLOW_STEP);
        assert_eq!(f.count("feedback_events"), 0);
        assert!(!absent.exists());
        assert_eq!(source.get_memory(&f.memory.id).unwrap(), before);
    }

    #[test]
    fn malformed_backflow_provenance_is_rejected_before_recording_global_feedback() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        f.destination.execute_raw(&format!("UPDATE memories SET provenance_uri = 'ee-mem://PRIVATE_CANARY/not-a-memory' WHERE id = '{id}'")).unwrap();
        let error = backflow_global_feedback(&backflow_options(&f, &id)).unwrap_err();
        assert!(!error.contains("PRIVATE_CANARY"));
        assert_eq!(f.count("feedback_events"), 0);
    }

    #[test]
    fn valid_but_foreign_backflow_provenance_cannot_adjust_or_audit_another_workspace() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let source = DbConnection::open_file(&f.source_path).unwrap();
        let before = source.get_memory(&f.memory.id).unwrap();
        let audits = source.count_table_rows("audit_log").unwrap();
        f.destination.execute_raw(&format!("UPDATE memories SET provenance_uri = 'ee-mem://wsp_00000000000000000000000091/{}' WHERE id = '{id}'", f.memory.id)).unwrap();
        let error = backflow_global_feedback(&backflow_options(&f, &id)).unwrap_err();
        assert!(error.contains("global_feedback_origin_pending"));
        assert_eq!(source.get_memory(&f.memory.id).unwrap(), before);
        assert_eq!(source.count_table_rows("audit_log").unwrap(), audits);
    }

    #[test]
    fn concurrent_origin_feedback_never_reports_success_for_a_lost_confidence_update() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let global = f.destination.get_memory(&id).unwrap().unwrap();
        let origin = (f.memory.workspace_id.clone(), f.memory.id.clone());
        let source = DbConnection::open_file_read_only(&f.source_path).unwrap();
        let before_audits = source.count_table_rows("audit_log").unwrap();
        let before_jobs = source.count_table_rows("search_index_jobs").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let successes = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let path = f.source_path.clone();
                    let paths = f.paths.clone();
                    let global = global.clone();
                    let origin = origin.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        let db = DbConnection::open_file(&path);
                        let options = BackflowOptions {
                            workspace_database_path: &path,
                            global_memory_id: &global.id,
                            global_paths: &paths,
                            signal: BackflowSignal::Helpful,
                            weight: 0.01,
                            actor: None,
                            dry_run: false,
                        };
                        barrier.wait();
                        let Ok(db) = db else {
                            return false;
                        };
                        persist_origin_backflow(
                            &db,
                            &options,
                            &global,
                            &origin,
                            &promotion_feedback_event_id(),
                            chrono::Utc::now(),
                        )
                        .is_ok_and(|change| change.is_some())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .filter(|success| *success)
                .count()
        });
        assert!(successes > 0);
        let current = source.get_memory(&f.memory.id).unwrap().unwrap();
        let expected = (0..successes).fold(f.memory.confidence, |value, _| value + 0.01);
        assert!((current.confidence - expected).abs() < 0.000001);
        assert_eq!(
            source.count_table_rows("audit_log").unwrap(),
            before_audits + i64::try_from(successes).unwrap()
        );
        assert_eq!(
            source.count_table_rows("search_index_jobs").unwrap(),
            before_jobs + i64::try_from(successes).unwrap()
        );
    }

    #[test]
    fn actual_backflow_steps_stay_within_the_cap_even_at_float_rounding_boundaries() {
        for before in [0.0_f32, 0.01, 0.49, 0.9, 0.99, 1.0] {
            for step in [0.0_f32, 0.000000001, MAX_BACKFLOW_STEP, -MAX_BACKFLOW_STEP] {
                let after = bounded_backflow_target(before, step);
                assert!((0.0..=1.0).contains(&after));
                assert!((f64::from(after) - f64::from(before)).abs() <= f64::from(step.abs()));
                assert!((after - before) * step >= 0.0);
            }
        }
    }

    #[test]
    fn malformed_origin_lifecycle_never_leaks_or_partially_updates_confidence() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let source = DbConnection::open_file(&f.source_path).unwrap();
        source
            .execute_raw(&format!(
                "UPDATE memories SET valid_to = 'PRIVATE_VALIDITY_CANARY' WHERE id = '{}'",
                f.memory.id
            ))
            .unwrap();
        let before = source.get_memory(&f.memory.id).unwrap();
        let jobs = source.count_table_rows("search_index_jobs").unwrap();
        let error = backflow_global_feedback(&backflow_options(&f, &id)).unwrap_err();
        assert!(!error.contains("PRIVATE_VALIDITY_CANARY"));
        assert_eq!(source.get_memory(&f.memory.id).unwrap(), before);
        assert_eq!(source.count_table_rows("search_index_jobs").unwrap(), jobs);
    }

    #[test]
    fn backflow_resumes_only_after_both_global_and_origin_seals_are_revealed() {
        let f = PublicationFixture::new();
        let (id, _, _) = f.publish().unwrap();
        let source = DbConnection::open_file(&f.source_path).unwrap();
        let commitment = crate::models::memory_seal_commitment(f.memory.content.as_bytes());
        f.destination
            .insert_memory_seal(&id, &commitment, "2020-01-01T00:00:00Z")
            .unwrap();
        source
            .insert_memory_seal(&f.memory.id, &commitment, "2020-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(
            backflow_global_feedback(&backflow_options(&f, &id))
                .unwrap()
                .applied_delta,
            0.0
        );
        assert!(
            f.destination
                .mark_memory_seal_revealed(&id, "2020-01-02T00:00:00Z")
                .unwrap()
        );
        assert_eq!(
            backflow_global_feedback(&backflow_options(&f, &id))
                .unwrap()
                .applied_delta,
            0.0
        );
        assert!(
            source
                .mark_memory_seal_revealed(&f.memory.id, "2020-01-02T00:00:00Z")
                .unwrap()
        );
        assert!(
            backflow_global_feedback(&backflow_options(&f, &id))
                .unwrap()
                .applied_delta
                > 0.0
        );
    }
}
