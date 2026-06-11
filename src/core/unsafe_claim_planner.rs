//! bd-1n3x1.16.2 — pure unsafe-claim blocker classifier.
//!
//! Maps the raw evidence of an unsafe claim-gate result (`unsafeReasons`
//! + `degradedCodes` from `ee.swarm.work_packet.claim_gate.v1`) onto the
//! deterministic reason-group categories and next-action families of
//! `docs/schemas/swarm/ee.swarm.unsafe_claim_plan.v1.json` (bd-1n3x1.16.1).
//!
//! Contract properties:
//!
//! - **Pure**: consumes already-gathered gate evidence; never re-runs
//!   git, Beads, BV, RCH, or Agent Mail.
//! - **Nothing dropped**: every raw reason lands in exactly one group
//!   (unknown inputs stay visible in the `unknown` group), preserved in
//!   bounded form with its original index for audit.
//! - **Deterministic**: group order follows the category ranking below;
//!   action families rank `stop` → `retry_with_snapshot` →
//!   `wait_or_coordinate` → `decompose_candidate` →
//!   `alternate_candidate` → `comment_template` → `inspect`; identical
//!   inputs produce identical output.
//! - **Advisory only**: every emitted action is non-mutating; the
//!   classifier can suggest a claim no more than the gate it consumes.

use serde::Serialize;

/// Maximum raw reason strings preserved per group; the remainder is
/// summarized by count so a pathological gate cannot bloat the plan.
const MAX_RAW_REASONS_PER_GROUP: usize = 16;
/// Maximum preserved length of one raw reason string.
const MAX_RAW_REASON_LEN: usize = 160;

/// Reason-group categories (ee.swarm.unsafe_claim_plan.v1 enum, in
/// deterministic rank order: the most claim-blocking authority problems
/// first, unknown always last).
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsafeClaimReasonCategory {
    TrackerAuthority,
    AgentMailReadiness,
    ReservationConflict,
    SourceOverlap,
    DirtyCheckout,
    RchProofAdmission,
    InstalledBinaryFreshness,
    BvStaleness,
    RecommendationMismatch,
    MemorySourceDrift,
    ResourceAdmission,
    ActionSuppression,
    Unknown,
}

impl UnsafeClaimReasonCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TrackerAuthority => "tracker_authority",
            Self::AgentMailReadiness => "agent_mail_readiness",
            Self::ReservationConflict => "reservation_conflict",
            Self::SourceOverlap => "source_overlap",
            Self::DirtyCheckout => "dirty_checkout",
            Self::RchProofAdmission => "rch_proof_admission",
            Self::InstalledBinaryFreshness => "installed_binary_freshness",
            Self::BvStaleness => "bv_staleness",
            Self::RecommendationMismatch => "recommendation_mismatch",
            Self::MemorySourceDrift => "memory_source_drift",
            Self::ResourceAdmission => "resource_admission",
            Self::ActionSuppression => "action_suppression",
            Self::Unknown => "unknown",
        }
    }

    /// Severity the category contributes to its group (gate evidence is
    /// advisory context; severities follow the shared low→critical
    /// vocabulary).
    #[must_use]
    pub const fn severity(self) -> &'static str {
        match self {
            Self::TrackerAuthority | Self::AgentMailReadiness | Self::ReservationConflict => "high",
            Self::SourceOverlap | Self::DirtyCheckout | Self::RchProofAdmission => "medium",
            Self::InstalledBinaryFreshness | Self::MemorySourceDrift | Self::ResourceAdmission => {
                "medium"
            }
            Self::BvStaleness | Self::RecommendationMismatch | Self::ActionSuppression => "low",
            Self::Unknown => "warning",
        }
    }

    const ALL: &'static [Self] = &[
        Self::TrackerAuthority,
        Self::AgentMailReadiness,
        Self::ReservationConflict,
        Self::SourceOverlap,
        Self::DirtyCheckout,
        Self::RchProofAdmission,
        Self::InstalledBinaryFreshness,
        Self::BvStaleness,
        Self::RecommendationMismatch,
        Self::MemorySourceDrift,
        Self::ResourceAdmission,
        Self::ActionSuppression,
        Self::Unknown,
    ];
}

/// Next-action families (ee.swarm.unsafe_claim_plan.v1 enum) in
/// deterministic rank order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsafeClaimActionKind {
    Stop,
    RetryWithSnapshot,
    WaitOrCoordinate,
    DecomposeCandidate,
    AlternateCandidate,
    CommentTemplate,
    Inspect,
}

impl UnsafeClaimActionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::RetryWithSnapshot => "retry_with_snapshot",
            Self::WaitOrCoordinate => "wait_or_coordinate",
            Self::DecomposeCandidate => "decompose_candidate",
            Self::AlternateCandidate => "alternate_candidate",
            Self::CommentTemplate => "comment_template",
            Self::Inspect => "inspect",
        }
    }
}

/// One classified group of raw gate evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimReasonGroup {
    pub category: UnsafeClaimReasonCategory,
    pub severity: &'static str,
    /// Bounded raw reason/degraded-code strings, classification order.
    pub reason_codes: Vec<String>,
    /// Indexes into the ORIGINAL concatenated evidence vector
    /// (unsafeReasons then degradedCodes), so audits can reconstruct
    /// exactly which raw entries this group consumed even past the
    /// preservation bound.
    pub raw_reason_indexes: Vec<usize>,
    /// Raw entries beyond [`MAX_RAW_REASONS_PER_GROUP`] are counted here
    /// rather than silently dropped.
    pub truncated_reason_count: usize,
    /// Human-oriented one-line summary of the group.
    pub bounded_preview: String,
}

/// One ranked advisory action.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimPlannerAction {
    pub kind: UnsafeClaimActionKind,
    pub rationale: String,
    /// Categories (by contract string) this action responds to.
    pub reason_group_refs: Vec<&'static str>,
    /// Always true: the classifier never authorizes mutation.
    pub advisory_only: bool,
    /// Always false: executing the SUGGESTION text is the agent's call.
    pub mutates_state: bool,
}

/// Classifier output: ordered groups plus ranked advisory actions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimClassification {
    pub reason_groups: Vec<UnsafeClaimReasonGroup>,
    pub planner_actions: Vec<UnsafeClaimPlannerAction>,
}

/// Map one raw gate evidence string to its category. Matching is by the
/// stable reason prefix (gate reasons carry `:`-joined detail suffixes).
#[must_use]
pub fn categorize_unsafe_claim_reason(reason: &str) -> UnsafeClaimReasonCategory {
    let head = reason.split(':').next().unwrap_or(reason).trim();
    match head {
        // Tracker authority: Beads reads are not trustworthy right now.
        "beads_tracker_not_authoritative"
        | "beads_tracker_stale"
        | "candidate_unresolved_due_to_tracker_state"
        | "actionable_queue_unavailable"
        | "actionable_queue_timed_out"
        | "actionable_queue_stale_fallback" => UnsafeClaimReasonCategory::TrackerAuthority,
        // Agent Mail readiness: coordination evidence missing or corrupt.
        "inbox_evidence_not_authoritative" => UnsafeClaimReasonCategory::AgentMailReadiness,
        // Reservations: someone may actively own the surface.
        "reservation_evidence_not_authoritative" | "reservation_collision" => {
            UnsafeClaimReasonCategory::ReservationConflict
        }
        // BV ranking staleness or contradiction.
        "bv_advisory_contradiction"
        | "bv_command_timeout"
        | "bv_no_output"
        | "bv_recommends_blocked_id" => UnsafeClaimReasonCategory::BvStaleness,
        // The gate could not line the candidate up with its own
        // recommendation — pick differently rather than force it.
        "candidate_not_found"
        | "candidate_decision"
        | "no_candidate_available"
        | "actionable_queue_candidate_absent"
        | "packet_recommendation_not_claim_safe"
        | "packet_recommendation_candidate_mismatch"
        | "packet_recommendation_candidate_missing"
        | "gate_verdict" => UnsafeClaimReasonCategory::RecommendationMismatch,
        "install_freshness" | "claim_gate_install_freshness_not_authoritative" => {
            UnsafeClaimReasonCategory::InstalledBinaryFreshness
        }
        _ => {
            if head.starts_with("agent_mail") {
                UnsafeClaimReasonCategory::AgentMailReadiness
            } else if head.starts_with("rch_") {
                UnsafeClaimReasonCategory::RchProofAdmission
            } else if head.starts_with("memory_drift") || head.starts_with("memory_probe") {
                UnsafeClaimReasonCategory::MemorySourceDrift
            } else if head.starts_with("source_overlap") || head.starts_with("file_surface") {
                UnsafeClaimReasonCategory::SourceOverlap
            } else if head.starts_with("dirty") || head.starts_with("workspace_hygiene") {
                UnsafeClaimReasonCategory::DirtyCheckout
            } else if head.starts_with("resource_") || head.starts_with("admission") {
                UnsafeClaimReasonCategory::ResourceAdmission
            } else if head.starts_with("action_suppress") || head.starts_with("suppressed_") {
                UnsafeClaimReasonCategory::ActionSuppression
            } else {
                UnsafeClaimReasonCategory::Unknown
            }
        }
    }
}

fn bounded_reason(reason: &str) -> String {
    if reason.len() <= MAX_RAW_REASON_LEN {
        reason.to_owned()
    } else {
        let mut cut = MAX_RAW_REASON_LEN;
        while !reason.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &reason[..cut])
    }
}

/// Classify gate evidence into ordered reason groups and ranked advisory
/// actions. `unsafe_reasons` and `degraded_codes` are consumed as one
/// evidence vector (reasons first), and every entry is preserved in
/// exactly one group.
#[must_use]
pub fn classify_unsafe_claim_evidence(
    unsafe_reasons: &[String],
    degraded_codes: &[String],
) -> UnsafeClaimClassification {
    let mut groups: Vec<UnsafeClaimReasonGroup> = Vec::new();
    for category in UnsafeClaimReasonCategory::ALL {
        groups.push(UnsafeClaimReasonGroup {
            category: *category,
            severity: category.severity(),
            reason_codes: Vec::new(),
            raw_reason_indexes: Vec::new(),
            truncated_reason_count: 0,
            bounded_preview: String::new(),
        });
    }

    for (index, reason) in unsafe_reasons
        .iter()
        .chain(degraded_codes.iter())
        .enumerate()
    {
        let category = categorize_unsafe_claim_reason(reason);
        let group = groups
            .iter_mut()
            .find(|group| group.category == category)
            .expect("every category has a pre-allocated group");
        group.raw_reason_indexes.push(index);
        if group.reason_codes.len() < MAX_RAW_REASONS_PER_GROUP {
            group.reason_codes.push(bounded_reason(reason));
        } else {
            group.truncated_reason_count += 1;
        }
    }

    groups.retain(|group| !group.raw_reason_indexes.is_empty());
    for group in &mut groups {
        group.bounded_preview = format!(
            "{} blocker(s) in {}{}",
            group.raw_reason_indexes.len(),
            group.category.as_str(),
            if group.truncated_reason_count > 0 {
                format!(
                    " ({} preserved by index only)",
                    group.truncated_reason_count
                )
            } else {
                String::new()
            }
        );
    }

    let planner_actions = rank_unsafe_claim_actions(&groups);
    UnsafeClaimClassification {
        reason_groups: groups,
        planner_actions,
    }
}

fn has_category(groups: &[UnsafeClaimReasonGroup], category: UnsafeClaimReasonCategory) -> bool {
    groups.iter().any(|group| group.category == category)
}

/// Rank advisory actions for the classified groups. One action per
/// applicable family, emitted in the fixed family order.
#[must_use]
pub fn rank_unsafe_claim_actions(
    groups: &[UnsafeClaimReasonGroup],
) -> Vec<UnsafeClaimPlannerAction> {
    use UnsafeClaimReasonCategory as Category;
    let mut actions = Vec::new();

    // stop: policy/suppression evidence — a human or policy gate said no.
    if has_category(groups, Category::ActionSuppression) {
        actions.push(UnsafeClaimPlannerAction {
            kind: UnsafeClaimActionKind::Stop,
            rationale: "An action-suppression or policy signal is present; do not work around \
                        it — surface it to a human."
                .to_owned(),
            reason_group_refs: vec![Category::ActionSuppression.as_str()],
            advisory_only: true,
            mutates_state: false,
        });
    }

    // retry_with_snapshot: missing Agent Mail evidence is bridgeable
    // read-only.
    if has_category(groups, Category::AgentMailReadiness) {
        actions.push(UnsafeClaimPlannerAction {
            kind: UnsafeClaimActionKind::RetryWithSnapshot,
            rationale: "Agent Mail evidence is missing or non-authoritative; generate a \
                        redacted ee.agent_mail.snapshot.v1 and retry the same claim gate with \
                        --agent-mail-snapshot (read-only evidence, not authorization)."
                .to_owned(),
            reason_group_refs: vec![Category::AgentMailReadiness.as_str()],
            advisory_only: true,
            mutates_state: false,
        });
    }

    // wait_or_coordinate: a peer may actively own the surface or the
    // shared substrate is mid-churn.
    {
        let mut refs = Vec::new();
        for category in [
            Category::ReservationConflict,
            Category::TrackerAuthority,
            Category::RchProofAdmission,
            Category::ResourceAdmission,
        ] {
            if has_category(groups, category) {
                refs.push(category.as_str());
            }
        }
        if !refs.is_empty() {
            actions.push(UnsafeClaimPlannerAction {
                kind: UnsafeClaimActionKind::WaitOrCoordinate,
                rationale: "Active reservations, tracker churn, or shared verification \
                            substrate is in motion; coordinate with the owner or wait for the \
                            evidence to settle instead of claiming through it."
                    .to_owned(),
                reason_group_refs: refs,
                advisory_only: true,
                mutates_state: false,
            });
        }
    }

    // decompose_candidate: broad source overlaps suggest the bead spans
    // surfaces other agents hold.
    {
        let mut refs = Vec::new();
        for category in [Category::SourceOverlap, Category::DirtyCheckout] {
            if has_category(groups, category) {
                refs.push(category.as_str());
            }
        }
        if !refs.is_empty() {
            actions.push(UnsafeClaimPlannerAction {
                kind: UnsafeClaimActionKind::DecomposeCandidate,
                rationale: "The candidate overlaps dirty or contested source surfaces; split \
                            it into disjoint leaves and claim only the surface no peer holds."
                    .to_owned(),
                reason_group_refs: refs,
                advisory_only: true,
                mutates_state: false,
            });
        }
    }

    // alternate_candidate: the gate itself recommends a different leaf.
    {
        let mut refs = Vec::new();
        for category in [Category::RecommendationMismatch, Category::BvStaleness] {
            if has_category(groups, category) {
                refs.push(category.as_str());
            }
        }
        if !refs.is_empty() {
            actions.push(UnsafeClaimPlannerAction {
                kind: UnsafeClaimActionKind::AlternateCandidate,
                rationale: "The gate's own recommendation does not line up with this \
                            candidate; pick from the actionable queue intersection instead of \
                            forcing this claim."
                    .to_owned(),
                reason_group_refs: refs,
                advisory_only: true,
                mutates_state: false,
            });
        }
    }

    // inspect: drift/freshness families need evidence-gathering before any
    // of the above is trustworthy.
    {
        let mut refs = Vec::new();
        for category in [
            Category::InstalledBinaryFreshness,
            Category::MemorySourceDrift,
            Category::Unknown,
        ] {
            if has_category(groups, category) {
                refs.push(category.as_str());
            }
        }
        if !refs.is_empty() {
            actions.push(UnsafeClaimPlannerAction {
                kind: UnsafeClaimActionKind::Inspect,
                rationale: "Freshness, drift, or unrecognized evidence is present; inspect \
                            (ee diag environment-attestation, ee doctor) before trusting any \
                            other action family."
                    .to_owned(),
                reason_group_refs: refs,
                advisory_only: true,
                mutates_state: false,
            });
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn classification_is_deterministic_and_loses_nothing() -> TestResult {
        let reasons = strings(&[
            "beads_tracker_not_authoritative:external_changes_pending_import",
            "candidate_not_found:bd-xyz",
            "gate_verdict:candidate_not_found",
            "packet_recommendation_not_claim_safe:coordinate_before_claim",
            "totally_new_future_reason:details",
        ]);
        let codes = strings(&[
            "agent_mail_unavailable",
            "beads_tracker_stale",
            "bv_command_timeout",
            "memory_drift_source_unverifiable",
            "memory_probe_unavailable",
        ]);
        let first = classify_unsafe_claim_evidence(&reasons, &codes);
        let second = classify_unsafe_claim_evidence(&reasons, &codes);
        ensure(
            first == second,
            "identical inputs must classify identically",
        )?;

        let preserved: usize = first
            .reason_groups
            .iter()
            .map(|group| group.raw_reason_indexes.len())
            .sum();
        ensure(
            preserved == reasons.len() + codes.len(),
            format!(
                "every raw entry must land in exactly one group, preserved {} of {}",
                preserved,
                reasons.len() + codes.len()
            ),
        )?;
        ensure(
            first.reason_groups.iter().any(|group| {
                group.category == UnsafeClaimReasonCategory::Unknown
                    && group
                        .reason_codes
                        .iter()
                        .any(|code| code.starts_with("totally_new_future_reason"))
            }),
            "unknown future reasons must remain visible in the unknown group",
        )?;

        let categories: Vec<_> = first
            .reason_groups
            .iter()
            .map(|group| group.category)
            .collect();
        let mut sorted = categories.clone();
        sorted.sort();
        ensure(
            categories == sorted,
            "groups must emit in the deterministic category rank order",
        )
    }

    #[test]
    fn action_families_rank_deterministically() -> TestResult {
        let reasons = strings(&[
            "reservation_evidence_not_authoritative",
            "agent_mail_unavailable",
            "candidate_not_found:bd-abc",
            "action_suppressed_by_policy",
        ]);
        let classification = classify_unsafe_claim_evidence(&reasons, &[]);
        let kinds: Vec<_> = classification
            .planner_actions
            .iter()
            .map(|action| action.kind)
            .collect();
        ensure(
            kinds
                == vec![
                    UnsafeClaimActionKind::Stop,
                    UnsafeClaimActionKind::RetryWithSnapshot,
                    UnsafeClaimActionKind::WaitOrCoordinate,
                    UnsafeClaimActionKind::AlternateCandidate,
                ],
            format!("family ranking drifted: {kinds:?}"),
        )?;
        ensure(
            classification
                .planner_actions
                .iter()
                .all(|action| action.advisory_only && !action.mutates_state),
            "every planner action must be advisory and non-mutating",
        )
    }

    #[test]
    fn snapshot_retry_is_offered_for_missing_agent_mail_evidence() -> TestResult {
        let classification =
            classify_unsafe_claim_evidence(&strings(&["agent_mail_unavailable"]), &[]);
        ensure(
            classification
                .planner_actions
                .iter()
                .any(|action| action.kind == UnsafeClaimActionKind::RetryWithSnapshot),
            "missing Agent Mail evidence must offer retry_with_snapshot",
        )
    }

    #[test]
    fn raw_preservation_is_bounded_but_indexed() -> TestResult {
        let many: Vec<String> = (0..40)
            .map(|index| format!("rch_capacity_blocker_{index:02}"))
            .collect();
        let classification = classify_unsafe_claim_evidence(&many, &[]);
        let group = classification
            .reason_groups
            .iter()
            .find(|group| group.category == UnsafeClaimReasonCategory::RchProofAdmission)
            .ok_or("rch group must exist")?;
        ensure(
            group.reason_codes.len() == MAX_RAW_REASONS_PER_GROUP,
            "raw preservation must stop at the bound",
        )?;
        ensure(
            group.truncated_reason_count == many.len() - MAX_RAW_REASONS_PER_GROUP,
            "entries past the bound must be counted, not dropped",
        )?;
        ensure(
            group.raw_reason_indexes.len() == many.len(),
            "every entry keeps its audit index even past the bound",
        )?;
        ensure(
            group.bounded_preview.contains("preserved by index only"),
            "the preview must disclose the truncation",
        )
    }

    #[test]
    fn category_mapping_covers_known_gate_vocabulary() -> TestResult {
        for (reason, expected) in [
            (
                "beads_tracker_not_authoritative:import_pending",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "agent_mail_recovery_corrupt",
                UnsafeClaimReasonCategory::AgentMailReadiness,
            ),
            (
                "reservation_collision:src/core/*.rs",
                UnsafeClaimReasonCategory::ReservationConflict,
            ),
            (
                "rch_verify_capacity_or_timeout",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "install_freshness:stale",
                UnsafeClaimReasonCategory::InstalledBinaryFreshness,
            ),
            (
                "bv_advisory_contradiction:bd-1",
                UnsafeClaimReasonCategory::BvStaleness,
            ),
            (
                "packet_recommendation_candidate_mismatch:bd-1:bd-2",
                UnsafeClaimReasonCategory::RecommendationMismatch,
            ),
            (
                "memory_probe_unavailable",
                UnsafeClaimReasonCategory::MemorySourceDrift,
            ),
            ("never_seen_before", UnsafeClaimReasonCategory::Unknown),
        ] {
            let got = categorize_unsafe_claim_reason(reason);
            ensure(
                got == expected,
                format!("{reason}: expected {expected:?}, got {got:?}"),
            )?;
        }
        Ok(())
    }
}
