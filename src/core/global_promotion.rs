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
