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
    /// Sealed-placeholder content: the body is withheld pending reveal.
    SealedPlaceholder,
    /// Trust class below the evidence gate.
    EvidenceGateTrustTooLow { trust_class: String },
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
            Self::EvidenceGateTrustTooLow { .. } => "global_promotion_evidence_gate",
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
            Self::EvidenceGateTrustTooLow { trust_class } => format!(
                "Promotion requires trust class human_explicit or agent_validated; this memory is `{trust_class}`."
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
            Self::EvidenceGateTrustTooLow { .. } => {
                "Validate the memory first (record outcome evidence or human confirmation), then retry."
                    .to_owned()
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
    if candidate.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT {
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
        })
    }
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
    let workspace_connection = DbConnection::open_file(options.workspace_database_path)
        .map_err(|error| format!("open workspace database: {error}"))?;
    let memory = workspace_connection
        .get_memory(options.memory_id)
        .map_err(|error| format!("load memory: {error}"))?
        .ok_or_else(|| format!("memory {} not found", options.memory_id))?;

    // Exact-content twin scan against the global store (deterministic v1
    // duplicate signal; similarity 1.0 by construction).
    let (global_connection, global_workspace_id) =
        super::global_store::open_or_create_global_store(options.global_paths)
            .map_err(|error| format!("open global store: {error}"))?;
    let existing_twin = global_connection
        .find_active_memory_by_content(&global_workspace_id, &memory.content)
        .map_err(|error| format!("scan global duplicates: {error}"))?;

    let plan = plan_promotion(&PromotionInput {
        candidate: PromotionCandidate {
            memory_id: memory.id.clone(),
            workspace_id: memory.workspace_id.clone(),
            content: memory.content.clone(),
            level: memory.level.clone(),
            kind: memory.kind.clone(),
            trust_class: memory.trust_class.clone(),
            confidence: memory.confidence,
            tombstoned: memory.tombstoned_at.is_some(),
        },
        nearest_global_duplicate: existing_twin.as_ref().map(|twin| GlobalNearDuplicate {
            global_memory_id: twin.id.clone(),
            similarity: 1.0,
        }),
        merge_similarity: None,
        global_lane_available: options.global_lane_available,
    });

    if !plan.allowed() || options.dry_run {
        let _ = global_connection.close();
        return Ok(PromotionReport {
            executed: false,
            global_memory_id: existing_twin.map(|twin| twin.id),
            already_promoted: false,
            plan,
        });
    }

    let (global_memory_id, already_promoted) = match &plan.verdict {
        PromotionVerdict::Allow {
            action: PromotionAction::MergeInto { global_memory_id },
        } => (global_memory_id.clone(), true),
        PromotionVerdict::Allow {
            action: PromotionAction::Insert,
        } => {
            let new_id = crate::models::MemoryId::now().to_string();
            let mut tags = vec!["scope:global".to_owned()];
            tags.push(format!("origin:{}", memory.workspace_id));
            global_connection
                .insert_memory(
                    &new_id,
                    &CreateMemoryInput {
                        workspace_id: global_workspace_id.clone(),
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
                        tags,
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| format!("insert global memory: {error}"))?;
            global_connection
                .insert_search_index_job(
                    &promotion_index_job_id(),
                    &CreateSearchIndexJobInput {
                        workspace_id: global_workspace_id.clone(),
                        job_type: SearchIndexJobType::SingleDocument,
                        document_source: Some("memory".to_owned()),
                        document_id: Some(new_id.clone()),
                        documents_total: 1,
                    },
                )
                .map_err(|error| format!("queue global index job: {error}"))?;
            (new_id, false)
        }
        PromotionVerdict::Refuse { .. } => unreachable!("allowed() checked above"),
    };

    // Audit in BOTH stores: the origin workspace records what left, the
    // global store records what arrived (no silent memory mutation).
    let details = json!({
        "schema": GLOBAL_PROMOTION_REPORT_SCHEMA_V1,
        "originWorkspaceId": memory.workspace_id,
        "originMemoryId": memory.id,
        "globalMemoryId": global_memory_id,
        "alreadyPromoted": already_promoted,
    })
    .to_string();
    let workspace_audit = CreateAuditInput {
        workspace_id: Some(memory.workspace_id.clone()),
        actor: options.actor.map(str::to_owned),
        action: plan.audit_action.to_owned(),
        target_type: Some("memory".to_owned()),
        target_id: Some(memory.id.clone()),
        details: Some(details.clone()),
    };
    workspace_connection
        .insert_audit(&generate_audit_id(), &workspace_audit)
        .map_err(|error| format!("workspace audit: {error}"))?;
    let global_audit = CreateAuditInput {
        workspace_id: Some(global_workspace_id),
        actor: options.actor.map(str::to_owned),
        action: plan.audit_action.to_owned(),
        target_type: Some("memory".to_owned()),
        target_id: Some(global_memory_id.clone()),
        details: Some(details),
    };
    global_connection
        .insert_audit(&generate_audit_id(), &global_audit)
        .map_err(|error| format!("global audit: {error}"))?;
    let _ = global_connection.close();
    let _ = workspace_connection.close();

    Ok(PromotionReport {
        plan,
        executed: true,
        global_memory_id: Some(global_memory_id),
        already_promoted,
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

/// Demote (tombstone) a global row. The origin workspace row is never
/// touched — demotion withdraws the global copy, it does not delete
/// knowledge.
///
/// # Errors
///
/// Returns a human-readable error string when storage access fails or the
/// global row does not exist.
pub fn demote_global(options: &DemoteGlobalOptions<'_>) -> Result<DemotionReport, String> {
    let (global_connection, global_workspace_id) =
        super::global_store::open_or_create_global_store(options.global_paths)
            .map_err(|error| format!("open global store: {error}"))?;
    let row = global_connection
        .get_memory(options.global_memory_id)
        .map_err(|error| format!("load global memory: {error}"))?
        .ok_or_else(|| format!("global memory {} not found", options.global_memory_id))?;
    let origin = row
        .provenance_uri
        .as_deref()
        .and_then(parse_promotion_provenance);

    if options.dry_run {
        let _ = global_connection.close();
        return Ok(DemotionReport {
            global_memory_id: row.id,
            executed: false,
            tombstoned: false,
            origin,
        });
    }

    let tombstoned = global_connection
        .tombstone_memory(&row.id)
        .map_err(|error| format!("tombstone global memory: {error}"))?;
    let details = json!({
        "schema": GLOBAL_DEMOTION_REPORT_SCHEMA_V1,
        "globalMemoryId": row.id,
        "originWorkspaceId": origin.as_ref().map(|(workspace, _)| workspace.clone()),
        "originMemoryId": origin.as_ref().map(|(_, memory)| memory.clone()),
    })
    .to_string();
    global_connection
        .insert_audit(
            &generate_audit_id(),
            &CreateAuditInput {
                workspace_id: Some(global_workspace_id),
                actor: options.actor.map(str::to_owned),
                action: "memory.demote_global".to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some(row.id.clone()),
                details: Some(details.clone()),
            },
        )
        .map_err(|error| format!("global audit: {error}"))?;
    if let Some((origin_workspace, origin_memory)) = &origin {
        if let Ok(workspace_connection) = DbConnection::open_file(options.workspace_database_path) {
            let _ = workspace_connection.insert_audit(
                &generate_audit_id(),
                &CreateAuditInput {
                    workspace_id: Some(origin_workspace.clone()),
                    actor: options.actor.map(str::to_owned),
                    action: "memory.demote_global".to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some(origin_memory.clone()),
                    details: Some(details),
                },
            );
            let _ = workspace_connection.close();
        }
    }
    let _ = global_connection.close();

    Ok(DemotionReport {
        global_memory_id: row.id,
        executed: true,
        tombstoned,
        origin,
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

/// Record outcome feedback against a global row and flow a bounded, audited
/// confidence adjustment back to the origin workspace row.
///
/// # Errors
///
/// Returns a human-readable error string when storage access fails or the
/// global row does not exist.
pub fn backflow_global_feedback(options: &BackflowOptions<'_>) -> Result<BackflowReport, String> {
    let (global_connection, global_workspace_id) =
        super::global_store::open_or_create_global_store(options.global_paths)
            .map_err(|error| format!("open global store: {error}"))?;
    let row = global_connection
        .get_memory(options.global_memory_id)
        .map_err(|error| format!("load global memory: {error}"))?
        .ok_or_else(|| format!("global memory {} not found", options.global_memory_id))?;
    let origin = row
        .provenance_uri
        .as_deref()
        .and_then(parse_promotion_provenance);

    let step = options.weight.clamp(0.0, MAX_BACKFLOW_STEP);
    let signed_delta = match options.signal {
        BackflowSignal::Helpful => step,
        BackflowSignal::Harmful => -step,
    };

    if options.dry_run {
        let _ = global_connection.close();
        return Ok(BackflowReport {
            global_memory_id: row.id,
            origin,
            applied_delta: signed_delta,
            origin_confidence_before: None,
            origin_confidence_after: None,
            executed: false,
        });
    }

    let now = chrono::Utc::now().to_rfc3339();
    // 1) Feedback event on the global row itself.
    global_connection
        .insert_feedback_event(
            &promotion_feedback_event_id(),
            &crate::db::CreateFeedbackEventInput {
                workspace_id: global_workspace_id.clone(),
                target_type: "memory".to_owned(),
                target_id: row.id.clone(),
                signal: options.signal.as_str().to_owned(),
                weight: step,
                source_type: "outcome_observed".to_owned(),
                source_id: options.actor.map(str::to_owned),
                reason: Some("global-lane outcome evidence (backflow)".to_owned()),
                evidence_json: None,
                session_id: None,
            },
        )
        .map_err(|error| format!("record global feedback: {error}"))?;

    // 2) Bounded origin adjustment, when this row was promoted.
    let (before, after) = if let Some((origin_workspace, origin_memory)) = &origin {
        let workspace_connection = DbConnection::open_file(options.workspace_database_path)
            .map_err(|error| format!("open workspace database: {error}"))?;
        let origin_row = workspace_connection
            .get_memory(origin_memory)
            .map_err(|error| format!("load origin memory: {error}"))?;
        let outcome = match origin_row {
            Some(origin_row) if origin_row.tombstoned_at.is_none() => {
                let before = origin_row.confidence;
                let target = (before + signed_delta).clamp(0.0, 1.0);
                let applied = workspace_connection
                    .apply_memory_reinforcement(origin_memory, origin_workspace, target, &now)
                    .map_err(|error| format!("adjust origin confidence: {error}"))?;
                let details = json!({
                    "schema": GLOBAL_BACKFLOW_REPORT_SCHEMA_V1,
                    "globalMemoryId": row.id,
                    "signal": options.signal.as_str(),
                    "appliedDelta": signed_delta,
                    "confidenceBefore": before,
                    "confidenceAfter": target,
                })
                .to_string();
                workspace_connection
                    .insert_audit(
                        &generate_audit_id(),
                        &CreateAuditInput {
                            workspace_id: Some(origin_workspace.clone()),
                            actor: options.actor.map(str::to_owned),
                            action: "memory.global_feedback_backflow".to_owned(),
                            target_type: Some("memory".to_owned()),
                            target_id: Some(origin_memory.clone()),
                            details: Some(details),
                        },
                    )
                    .map_err(|error| format!("origin audit: {error}"))?;
                applied.then_some((before, target))
            }
            // Origin tombstoned or vanished: feedback stays on the global
            // row only; never resurrect or adjust dead rows.
            _ => None,
        };
        let _ = workspace_connection.close();
        match outcome {
            Some((before, after)) => (Some(before), Some(after)),
            None => (None, None),
        }
    } else {
        (None, None)
    };
    let _ = global_connection.close();

    Ok(BackflowReport {
        global_memory_id: row.id,
        origin,
        applied_delta: signed_delta,
        origin_confidence_before: before,
        origin_confidence_after: after,
        executed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn tombstoned_sealed_and_lane_off_refuse() {
        let mut dead = input(candidate("human_explicit"));
        dead.candidate.tombstoned = true;
        assert!(!plan_promotion(&dead).allowed());

        let mut sealed = input(candidate("human_explicit"));
        sealed.candidate.content = crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT.to_owned();
        let sealed_plan = plan_promotion(&sealed);
        let PromotionVerdict::Refuse { refusal } = &sealed_plan.verdict else {
            panic!("sealed placeholder must refuse");
        };
        assert_eq!(refusal.code(), "global_promotion_sealed");
        assert!(refusal.repair().contains("ee memory reveal"));

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
}
