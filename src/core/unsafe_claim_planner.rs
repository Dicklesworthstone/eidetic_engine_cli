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
//! - **Deterministic**: group and action order follow the schema enum
//!   rankings in `ee.swarm.unsafe_claim_plan.v1`; identical inputs produce
//!   identical output.
//! - **Advisory only**: every emitted action is non-mutating; the
//!   classifier can suggest a claim no more than the gate it consumes.

use serde::Serialize;

/// Maximum raw reason strings preserved per group; the remainder is
/// summarized by count so a pathological gate cannot bloat the plan.
const MAX_RAW_REASONS_PER_GROUP: usize = 16;
/// Maximum preserved length of one raw reason string.
const MAX_RAW_REASON_LEN: usize = 160;
const TRUNCATED_REASON_MARKER: &str = "…";

/// Reason-group categories (ee.swarm.unsafe_claim_plan.v1 enum, in
/// deterministic schema order).
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsafeClaimReasonCategory {
    TrackerAuthority,
    AgentMailReadiness,
    SourceOverlap,
    DirtyCheckout,
    RchProofAdmission,
    InstalledBinaryFreshness,
    ReservationConflict,
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
            Self::TrackerAuthority | Self::ReservationConflict => "high",
            Self::AgentMailReadiness
            | Self::SourceOverlap
            | Self::RchProofAdmission
            | Self::InstalledBinaryFreshness
            | Self::MemorySourceDrift
            | Self::ResourceAdmission => "medium",
            Self::DirtyCheckout | Self::BvStaleness => "warning",
            Self::RecommendationMismatch | Self::ActionSuppression => "low",
            Self::Unknown => "warning",
        }
    }

    #[must_use]
    pub const fn rank(self) -> usize {
        match self {
            Self::TrackerAuthority => 0,
            Self::AgentMailReadiness => 1,
            Self::SourceOverlap => 2,
            Self::DirtyCheckout => 3,
            Self::RchProofAdmission => 4,
            Self::InstalledBinaryFreshness => 5,
            Self::ReservationConflict => 6,
            Self::BvStaleness => 7,
            Self::RecommendationMismatch => 8,
            Self::MemorySourceDrift => 9,
            Self::ResourceAdmission => 10,
            Self::ActionSuppression => 11,
            Self::Unknown => 12,
        }
    }

    /// Reverse of [`Self::as_str`]: resolve a contract category string back to
    /// its variant (used to map an action's reason-group refs to evidence ids).
    #[must_use]
    fn from_contract_str(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|category| category.as_str() == value)
    }

    const ALL: &'static [Self] = &[
        Self::TrackerAuthority,
        Self::AgentMailReadiness,
        Self::SourceOverlap,
        Self::DirtyCheckout,
        Self::RchProofAdmission,
        Self::InstalledBinaryFreshness,
        Self::ReservationConflict,
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
    Inspect,
    CommentTemplate,
    DecomposeCandidate,
    AlternateCandidate,
    RetryWithSnapshot,
    WaitOrCoordinate,
    Stop,
}

impl UnsafeClaimActionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::CommentTemplate => "comment_template",
            Self::DecomposeCandidate => "decompose_candidate",
            Self::AlternateCandidate => "alternate_candidate",
            Self::RetryWithSnapshot => "retry_with_snapshot",
            Self::WaitOrCoordinate => "wait_or_coordinate",
            Self::Stop => "stop",
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
        | "tracker_not_authoritative"
        | "beads_requires_candidate_downgrade"
        | "beads_db_jsonl_count_mismatch"
        | "beads_unavailable"
        | "beads_command_timeout"
        | "beads_no_output"
        | "beads_metadata_only_stale"
        | "beads_tracker_metadata_drift"
        | "ready_unclaimed_visible_but_not_authoritative"
        | "beads_ready_source_stale"
        | "candidate_unresolved_due_to_tracker_state"
        | "tracker_authority_degraded"
        | "actionable_queue_unavailable"
        | "actionable_queue_timed_out"
        | "actionable_queue_stale_fallback" => UnsafeClaimReasonCategory::TrackerAuthority,
        // Agent Mail readiness: coordination evidence missing or corrupt.
        // `archive_corruption` and `green_transport_does_not_imply_authoritative_reads`
        // are live work-packet coordination strings emitted when Agent Mail
        // recovery is corrupt or semantic readiness failed (bd-1n3x1.16.7).
        "inbox_evidence_not_authoritative"
        | "archive_index_parity_drift"
        | "archive_corruption"
        | "green_transport_does_not_imply_authoritative_reads" => {
            UnsafeClaimReasonCategory::AgentMailReadiness
        }
        // Reservations: someone may actively own the surface.
        "reservation_evidence_not_authoritative"
        | "reservation_evidence_stale"
        | "reservation_collision"
        | "active_claim"
        | "fallback_row_already_owned"
        | "candidate_assigned_to"
        | "active_owner_or_compile_health_blocker_present"
        | "reserved_file_overlap" => UnsafeClaimReasonCategory::ReservationConflict,
        // Same-file source proof debt (bd-1n3x1.15 `SAME_FILE_PROOF_DEBT_REASON`):
        // the candidate would edit a file already changed by source-complete but
        // Cargo-unproved peer work. That is a source overlap (decompose into the
        // surface the peer does not hold), not a reservation lease (bd-1n3x1.16.7).
        "unproved_same_file_source_debt" => UnsafeClaimReasonCategory::SourceOverlap,
        // BV ranking staleness or contradiction.
        "bv_advisory_contradiction"
        | "bv_command_timeout"
        | "bv_no_output"
        | "bv_unavailable"
        | "bv_recommendation_stale"
        | "bv_recommends_blocked_id"
        | "bv_recommends_id_absent_from_actionable_queue"
        | "graph_triage_unavailable" => UnsafeClaimReasonCategory::BvStaleness,
        // The gate could not line the candidate up with its own
        // recommendation — pick differently rather than force it.
        "candidate_not_found"
        | "candidate_decision"
        | "candidate_already_appears_in_multiple_sources"
        | "candidate_status"
        | "candidate_is_rollup_not_leaf"
        | "candidate_issue_type"
        | "rollup_candidate_not_claimable"
        | "claim_concrete_child_bead_instead"
        | "rollup_has_no_claimable_child"
        | "blocked_by"
        | "no_candidate_available"
        | "actionable_queue_candidate_absent"
        | "packet_recommendation_not_claim_safe"
        | "packet_recommendation_candidate_mismatch"
        | "packet_recommendation_candidate_missing"
        | "gate_verdict" => UnsafeClaimReasonCategory::RecommendationMismatch,
        "install_freshness"
        | "claim_gate_install_freshness_not_authoritative"
        | "stale_binary_suspected"
        | "stale_claim_gate_binary"
        | "unsupported_claim_gate_binary"
        | "missing_required_surface" => UnsafeClaimReasonCategory::InstalledBinaryFreshness,
        // Pre-inspection memory-drift failures are local resource/admission
        // gaps, not evidence that was inspected and found stale or invalid.
        "memory_drift_lock_contention" | "memory_drift_report_unavailable" => {
            UnsafeClaimReasonCategory::ResourceAdmission
        }
        "dirty_compile_health_blocks_rch"
        | "active_project_exclusion"
        | "all_workers_preflight_failed"
        | "build_admission_blocked"
        | "capacity_or_timeout"
        | "command_not_offloaded"
        | "insufficient_slots"
        | "no_admissible_workers"
        | "no_worker_selected"
        | "no_workers_passed_health"
        | "no_workers_with_rust_installed"
        | "remote_marker_missing"
        | "recent_verifier_evidence_available"
        | "recent_verifier_path"
        | "recent_verifier_reason"
        | "recent_verifier_status"
        | "recent_verifier_command_hash"
        | "recent_verifier_command_kind"
        | "recent_verifier_command_target"
        | "selector_admission_failed"
        | "topology_blocked"
        | "worker_health_threshold" => UnsafeClaimReasonCategory::RchProofAdmission,
        _ => {
            if let Some(category) = source_authority_reason_category(head) {
                category
            } else if head.starts_with("agent_mail") {
                UnsafeClaimReasonCategory::AgentMailReadiness
            } else if head.starts_with("rch_") {
                UnsafeClaimReasonCategory::RchProofAdmission
            } else if head.starts_with("memory_drift") || head.starts_with("memory_probe") {
                UnsafeClaimReasonCategory::MemorySourceDrift
            } else if head.starts_with("source_overlap")
                || head.starts_with("file_surface")
                || head.starts_with("file_collision")
                || head.starts_with("related_bead_collision")
                || head.starts_with("high_risk_dirty_surface")
            {
                UnsafeClaimReasonCategory::SourceOverlap
            } else if head.starts_with("dirty") || head.starts_with("workspace_hygiene") {
                UnsafeClaimReasonCategory::DirtyCheckout
            } else if head.starts_with("resource_")
                || head.starts_with("admission")
                || head.starts_with("disk_pressure")
                || head == "cache_pressure"
            {
                UnsafeClaimReasonCategory::ResourceAdmission
            } else if head.starts_with("action_suppress")
                || head.starts_with("suppressed_")
                || head.starts_with("release_operator_required")
                || head == "local_cargo_bypass_detected"
            {
                UnsafeClaimReasonCategory::ActionSuppression
            } else {
                UnsafeClaimReasonCategory::Unknown
            }
        }
    }
}

fn source_authority_reason_category(head: &str) -> Option<UnsafeClaimReasonCategory> {
    let tail = head.strip_prefix("source_authority_")?;
    if tail.starts_with("actionable_queue_") || tail.starts_with("beads_") {
        Some(UnsafeClaimReasonCategory::TrackerAuthority)
    } else if tail.starts_with("agent_mail_") {
        Some(UnsafeClaimReasonCategory::AgentMailReadiness)
    } else if tail.starts_with("bv_") {
        Some(UnsafeClaimReasonCategory::BvStaleness)
    } else if tail.starts_with("git_") || tail.starts_with("workspace_hygiene_") {
        Some(UnsafeClaimReasonCategory::DirtyCheckout)
    } else if tail.starts_with("rch_") {
        Some(UnsafeClaimReasonCategory::RchProofAdmission)
    } else if tail.starts_with("installed_binary_") {
        Some(UnsafeClaimReasonCategory::InstalledBinaryFreshness)
    } else if matches!(
        tail,
        "memory_drift_lock_contention" | "memory_drift_report_unavailable"
    ) {
        Some(UnsafeClaimReasonCategory::ResourceAdmission)
    } else if tail.starts_with("memory_drift_") {
        Some(UnsafeClaimReasonCategory::MemorySourceDrift)
    } else if tail.starts_with("host_profile_") || tail.starts_with("support_bundle_") {
        Some(UnsafeClaimReasonCategory::ResourceAdmission)
    } else {
        Some(UnsafeClaimReasonCategory::Unknown)
    }
}

fn bounded_reason(reason: &str) -> String {
    if reason.len() <= MAX_RAW_REASON_LEN {
        reason.to_owned()
    } else {
        let mut cut = MAX_RAW_REASON_LEN.saturating_sub(TRUNCATED_REASON_MARKER.len());
        while !reason.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}{}", &reason[..cut], TRUNCATED_REASON_MARKER)
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
        // Groups are pre-allocated in ALL order, so the category's rank is
        // its index — no fallible lookup needed.
        let rank = category.rank();
        let group = &mut groups[rank];
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
    use UnsafeClaimReasonCategory as UnsafeCategory;
    let mut actions = Vec::new();

    // inspect: drift/freshness families need evidence-gathering before any
    // other action family is trustworthy.
    {
        let mut refs = Vec::new();
        for category in [
            UnsafeCategory::InstalledBinaryFreshness,
            UnsafeCategory::MemorySourceDrift,
            UnsafeCategory::Unknown,
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

    // comment_template is emitted by the full plan projection once it can
    // include a bounded body template. This pure classifier has no body text
    // source, so it intentionally skips that action kind while preserving the
    // schema order for all emitted families.

    // decompose_candidate: broad source overlaps suggest the bead spans
    // surfaces other agents hold.
    {
        let mut refs = Vec::new();
        for category in [UnsafeCategory::SourceOverlap, UnsafeCategory::DirtyCheckout] {
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
        for category in [
            UnsafeCategory::RecommendationMismatch,
            UnsafeCategory::BvStaleness,
        ] {
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

    // retry_with_snapshot: missing Agent Mail evidence is bridgeable
    // read-only.
    if has_category(groups, UnsafeCategory::AgentMailReadiness) {
        actions.push(UnsafeClaimPlannerAction {
            kind: UnsafeClaimActionKind::RetryWithSnapshot,
            rationale: "Agent Mail evidence is missing or non-authoritative; generate a \
                        redacted ee.agent_mail.snapshot.v1 and retry the same claim gate with \
                        --agent-mail-snapshot (read-only evidence, not authorization)."
                .to_owned(),
            reason_group_refs: vec![UnsafeCategory::AgentMailReadiness.as_str()],
            advisory_only: true,
            mutates_state: false,
        });
    }

    // wait_or_coordinate: a peer may actively own the surface or the
    // shared substrate is mid-churn.
    {
        let mut refs = Vec::new();
        for category in [
            UnsafeCategory::ReservationConflict,
            UnsafeCategory::TrackerAuthority,
            UnsafeCategory::RchProofAdmission,
            UnsafeCategory::ResourceAdmission,
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

    // stop: policy/suppression evidence — a human or policy gate said no.
    if has_category(groups, UnsafeCategory::ActionSuppression) {
        actions.push(UnsafeClaimPlannerAction {
            kind: UnsafeClaimActionKind::Stop,
            rationale: "An action-suppression or policy signal is present; do not work around \
                        it — surface it to a human."
                .to_owned(),
            reason_group_refs: vec![UnsafeCategory::ActionSuppression.as_str()],
            advisory_only: true,
            mutates_state: false,
        });
    }

    actions
}

// ───────────────────────────────────────────────────────────────────────────
// bd-1n3x1.16.3 — non-mutating decomposition suggester.
//
// When a candidate is REAL work but unsafe to claim, suggest smaller
// self-contained leaves and a plan-space comment an agent may review and
// apply with br manually. Never calls br, never mutates dependencies,
// never marks anything in progress.
// ───────────────────────────────────────────────────────────────────────────

/// Non-secret candidate facts the suggester may consume (gate evidence
/// already gathered upstream; nothing here re-runs a source).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnsafeClaimCandidateFacts {
    pub candidate_id: String,
    pub title: String,
    /// Bounded description text (caller truncates; only used for breadth
    /// heuristics and keyword extraction, never echoed raw into output).
    pub description: String,
    pub issue_type: String,
    pub priority: Option<i64>,
    pub labels: Vec<String>,
    /// Bounded path families from the gate's source-overlap evidence
    /// (for example "src/db/", "src/cli/", "tests/").
    pub path_families: Vec<String>,
}

/// One suggested leaf bead, emitted as a draft for manual `br create`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedBeadDraft {
    pub title: String,
    pub issue_type: &'static str,
    pub priority: i64,
    pub labels: Vec<String>,
    pub parent_id: String,
    /// Human dependency hint ("create blocked by <prior leaf>"), never a
    /// machine mutation.
    pub dependency_hint: String,
    pub rationale: String,
}

/// Decomposition plan: advisory output only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimDecompositionPlan {
    /// True only when the evidence actually supports splitting.
    pub decompose: bool,
    /// Why decomposition is (or is not) recommended.
    pub reason: String,
    pub suggested_beads: Vec<SuggestedBeadDraft>,
    /// Plan-space comment citing gate blocker categories without raw logs.
    pub comment_template: String,
    /// Search terms for de-duplicating against existing beads before
    /// creating anything.
    pub overlap_search_terms: Vec<String>,
}

const BROAD_DESCRIPTION_CHARS: usize = 600;

fn candidate_is_broad(facts: &UnsafeClaimCandidateFacts) -> bool {
    matches!(facts.issue_type.as_str(), "feature" | "epic")
        || facts.title.starts_with("[idea-wizard]")
        || facts.title.starts_with("[CLUSTER]")
        || facts.title.starts_with("[THEME]")
        || facts.description.len() >= BROAD_DESCRIPTION_CHARS
        || facts.path_families.len() > 1
}

/// Deterministic surface lane for a path family.
fn lane_for_path_family(family: &str) -> (&'static str, &'static str) {
    let normalized = family.trim_start_matches("./");
    if normalized.starts_with("src/db") {
        ("storage", "schema/storage surface")
    } else if normalized.starts_with("src/cli") {
        ("cli", "CLI wiring surface")
    } else if normalized.starts_with("src/core") {
        ("domain", "core domain surface")
    } else if normalized.starts_with("tests") {
        ("tests", "test/golden surface")
    } else if normalized.starts_with("docs") {
        ("docs", "docs/schema surface")
    } else if normalized.starts_with("scripts") {
        ("scripts", "script/gate surface")
    } else {
        ("slice", "separable surface")
    }
}

const OVERLAP_STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "into", "from", "that", "this", "are", "not", "its", "their",
    "when", "where", "every", "all",
];

fn overlap_terms(facts: &UnsafeClaimCandidateFacts) -> Vec<String> {
    let mut terms = vec![facts.candidate_id.clone()];
    let mut seen = std::collections::BTreeSet::new();
    for word in facts.title.split(|c: char| !c.is_ascii_alphanumeric()) {
        let lowered = word.to_ascii_lowercase();
        if lowered.len() >= 4
            && !OVERLAP_STOPWORDS.contains(&lowered.as_str())
            && seen.insert(lowered.clone())
        {
            terms.push(lowered);
        }
        if terms.len() >= 8 {
            break;
        }
    }
    terms
}

/// Suggest a non-mutating decomposition plan for an unsafe-but-real
/// candidate. Decomposition is recommended only when the evidence
/// supports it: a broad candidate AND blocker categories that splitting
/// can actually route around (source overlap / dirty checkout). Narrow
/// leaves, or blockers that splitting cannot fix (tracker authority,
/// missing Agent Mail evidence alone), produce `decompose: false` with
/// an explanatory reason — never a make-work split.
#[must_use]
pub fn suggest_unsafe_claim_decomposition(
    facts: &UnsafeClaimCandidateFacts,
    classification: &UnsafeClaimClassification,
) -> UnsafeClaimDecompositionPlan {
    let split_routable = classification.reason_groups.iter().any(|group| {
        matches!(
            group.category,
            UnsafeClaimReasonCategory::SourceOverlap
                | UnsafeClaimReasonCategory::DirtyCheckout
                | UnsafeClaimReasonCategory::ReservationConflict
        )
    });
    let broad = candidate_is_broad(facts);

    if !split_routable {
        return UnsafeClaimDecompositionPlan {
            decompose: false,
            reason: "No source-overlap, dirty-checkout, or reservation blockers are present; \
                     splitting this candidate would not route around the gate's actual \
                     blockers."
                .to_owned(),
            suggested_beads: Vec::new(),
            comment_template: String::new(),
            overlap_search_terms: overlap_terms(facts),
        };
    }
    if !broad {
        return UnsafeClaimDecompositionPlan {
            decompose: false,
            reason: "The candidate is already a narrow leaf; the contested surface IS the \
                     work. Wait for or coordinate with the surface owner instead of splitting."
                .to_owned(),
            suggested_beads: Vec::new(),
            comment_template: String::new(),
            overlap_search_terms: overlap_terms(facts),
        };
    }

    let priority = facts.priority.unwrap_or(2);
    let decomposed_label = format!("decomposed-from:{}", facts.candidate_id);
    let mut suggested_beads = Vec::new();
    let mut lanes_seen = std::collections::BTreeSet::new();

    for family in &facts.path_families {
        let (lane, lane_description) = lane_for_path_family(family);
        if !lanes_seen.insert(lane) {
            continue;
        }
        let dependency_hint = if suggested_beads.is_empty() {
            "first leaf; later leaves should be created blocked by it".to_owned()
        } else {
            format!(
                "create blocked by the previous leaf so surfaces land in order ({} leaves so far)",
                suggested_beads.len()
            )
        };
        suggested_beads.push(SuggestedBeadDraft {
            title: format!("{}: {lane} slice — {}", facts.candidate_id, family),
            issue_type: "task",
            priority,
            labels: vec![decomposed_label.clone(), format!("lane:{lane}")],
            parent_id: facts.candidate_id.clone(),
            dependency_hint,
            rationale: format!(
                "Separable {lane_description} under {family}; claimable independently once \
                 the contested surfaces are split apart."
            ),
        });
    }
    // A broad bead with no concrete path families still gets the standard
    // design → implementation → proof ladder.
    if suggested_beads.is_empty() {
        for (lane, title_suffix, rationale) in [
            (
                "contract",
                "define contract/design leaf",
                "Pin the response contract or design decisions first; this leaf needs no \
                 contested source surface.",
            ),
            (
                "implementation",
                "implementation leaf on uncontested surfaces",
                "Implement against the pinned contract, scoped to surfaces no peer holds.",
            ),
            (
                "proof",
                "tests + RCH proof leaf",
                "Land verification separately so the implementation leaf stays small.",
            ),
        ] {
            let dependency_hint = if suggested_beads.is_empty() {
                "first leaf; later leaves should be created blocked by it".to_owned()
            } else {
                "create blocked by the previous leaf".to_owned()
            };
            suggested_beads.push(SuggestedBeadDraft {
                title: format!("{}: {title_suffix}", facts.candidate_id),
                issue_type: "task",
                priority,
                labels: vec![decomposed_label.clone(), format!("lane:{lane}")],
                parent_id: facts.candidate_id.clone(),
                dependency_hint,
                rationale: rationale.to_owned(),
            });
        }
    }

    let categories: Vec<&str> = classification
        .reason_groups
        .iter()
        .map(|group| group.category.as_str())
        .collect();
    let comment_template = format!(
        "Decomposition proposal for {candidate} (advisory, not yet applied): the claim gate \
         reports blockers in [{categories}], and the candidate spans {lanes} separable \
         surface(s). Splitting lets disjoint leaves proceed while contested surfaces wait for \
         their owners. Suggested leaves are listed in plan space; before creating any, search \
         existing beads for the overlap terms to avoid duplicates. Raw gate evidence stays in \
         the gate output — not repeated here.",
        candidate = facts.candidate_id,
        categories = categories.join(", "),
        lanes = suggested_beads.len(),
    );

    UnsafeClaimDecompositionPlan {
        decompose: true,
        reason: format!(
            "Broad candidate ({} type, {} path families) with split-routable blockers.",
            facts.issue_type,
            facts.path_families.len()
        ),
        suggested_beads,
        comment_template,
        overlap_search_terms: overlap_terms(facts),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// bd-1n3x1.16.4 — non-mutating alternate-candidate recommender.
//
// This slice consumes already-collected actionable-queue/BV/gate facts and
// ranks plausible alternates. It never shells out, never executes BV/Beads
// claim commands, and never turns an unsafe candidate into a safe one.
// ───────────────────────────────────────────────────────────────────────────

const MAX_ALTERNATE_RECOMMENDATIONS: usize = 8;
const MAX_CANDIDATE_DELTAS: usize = 12;

/// Candidate states from `ee.swarm.unsafe_claim_plan.v1`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsafeClaimAlternateCandidateState {
    FreshSafeToClaim,
    PlausibleButRequiresGate,
    ScannedAndUnsafe,
    NotFound,
    BlockedOrOwned,
    Unknown,
}

impl UnsafeClaimAlternateCandidateState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FreshSafeToClaim => "fresh_safe_to_claim",
            Self::PlausibleButRequiresGate => "plausible_but_requires_gate",
            Self::ScannedAndUnsafe => "scanned_and_unsafe",
            Self::NotFound => "not_found",
            Self::BlockedOrOwned => "blocked_or_owned",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn rank(self) -> usize {
        match self {
            Self::FreshSafeToClaim => 0,
            Self::PlausibleButRequiresGate => 1,
            Self::ScannedAndUnsafe => 2,
            Self::BlockedOrOwned => 3,
            Self::NotFound => 4,
            Self::Unknown => 5,
        }
    }
}

/// Coarse work class used only for advisory ordering when source authority
/// is degraded.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsafeClaimAlternateWorkClass {
    TrackerOnly,
    DocsOnly,
    ContractDesign,
    FixtureOnly,
    ShellOnlyNoCargo,
    RustSource,
    Unknown,
}

impl UnsafeClaimAlternateWorkClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TrackerOnly => "tracker_only",
            Self::DocsOnly => "docs_only",
            Self::ContractDesign => "contract_design",
            Self::FixtureOnly => "fixture_only",
            Self::ShellOnlyNoCargo => "shell_only_no_cargo",
            Self::RustSource => "rust_source",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn degraded_authority_rank(self) -> usize {
        match self {
            Self::TrackerOnly => 0,
            Self::DocsOnly => 1,
            Self::ContractDesign => 2,
            Self::FixtureOnly => 3,
            Self::ShellOnlyNoCargo => 4,
            Self::Unknown => 5,
            Self::RustSource => 6,
        }
    }
}

/// Pure input context for alternate recommendation. These fields are copied
/// from sources the caller already gathered.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnsafeClaimAlternatePlannerInput {
    pub requested_candidate_id: Option<String>,
    pub source_authority_degraded: bool,
    pub tracker_health: String,
    pub agent_mail_status: String,
    pub source_freshness: String,
    pub unsafe_path_families: Vec<String>,
    pub candidates: Vec<UnsafeClaimAlternateCandidateFacts>,
}

/// Non-secret candidate facts from the actionable queue, BV summary, or a
/// previously scanned claim gate.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnsafeClaimAlternateCandidateFacts {
    pub candidate_id: String,
    pub title: String,
    pub issue_type: String,
    pub status: String,
    pub assignee: Option<String>,
    pub priority: Option<i64>,
    /// Higher is better. Callers may pass BV score scaled to an integer.
    pub score: i64,
    pub labels: Vec<String>,
    pub path_families: Vec<String>,
    pub gate_verdict: Option<String>,
    pub gate_safe_to_claim: Option<bool>,
    pub gate_claim_command_action_present: bool,
    pub evidence_freshness: String,
    pub reason_group_refs: Vec<String>,
    pub candidate_specific_deltas: Vec<String>,
}

/// Read-only command action shape aligned with the unsafe-plan schema.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimReadOnlyCommandAction {
    pub command_id: String,
    pub display_command: String,
    pub argv: Vec<String>,
    pub shell_required: bool,
    pub copy_safety: &'static str,
    pub mutates_state: bool,
    pub required_substrate: &'static str,
    pub when: String,
    pub rationale: String,
}

/// One ranked alternate candidate recommendation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimAlternateRecommendation {
    pub rank: usize,
    pub candidate_id: String,
    pub candidate_state: UnsafeClaimAlternateCandidateState,
    pub work_class: UnsafeClaimAlternateWorkClass,
    pub gate_verdict: String,
    pub safe_to_claim: bool,
    pub priority: i64,
    pub score: i64,
    pub reason_group_refs: Vec<String>,
    pub candidate_specific_deltas: Vec<String>,
    pub needs_fresh_claim_gate: bool,
    /// Always false in this unsafe-plan projection. If a fresh gate already
    /// carries a claim command, consumers should use that gate directly.
    pub may_emit_claim_command: bool,
    pub advisory_notes: Vec<String>,
    pub next_command_actions: Vec<UnsafeClaimReadOnlyCommandAction>,
}

/// Bounded alternate recommendation output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimAlternateRecommendationPlan {
    pub recommended_action: String,
    pub source_authority_degraded: bool,
    pub candidates: Vec<UnsafeClaimAlternateRecommendation>,
    pub next_command_actions: Vec<UnsafeClaimReadOnlyCommandAction>,
}

fn normalized_path_family(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .trim_matches('/')
        .to_owned()
}

fn path_family_overlaps(left: &str, right: &str) -> bool {
    let left = normalized_path_family(left);
    let right = normalized_path_family(right);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left == right
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
}

fn candidate_overlaps_unsafe_surface(
    candidate: &UnsafeClaimAlternateCandidateFacts,
    unsafe_path_families: &[String],
) -> Option<String> {
    for candidate_path in &candidate.path_families {
        for unsafe_path in unsafe_path_families {
            if path_family_overlaps(candidate_path, unsafe_path) {
                return Some(normalized_path_family(candidate_path));
            }
        }
    }
    None
}

fn has_label_or_title(candidate: &UnsafeClaimAlternateCandidateFacts, needle: &str) -> bool {
    candidate
        .labels
        .iter()
        .any(|label| label.to_ascii_lowercase().contains(needle))
        || candidate.title.to_ascii_lowercase().contains(needle)
}

fn all_paths_match(
    candidate: &UnsafeClaimAlternateCandidateFacts,
    predicate: impl Fn(&str) -> bool,
) -> bool {
    !candidate.path_families.is_empty()
        && candidate
            .path_families
            .iter()
            .map(|path| normalized_path_family(path))
            .all(|path| predicate(&path))
}

fn classify_alternate_work(
    candidate: &UnsafeClaimAlternateCandidateFacts,
) -> UnsafeClaimAlternateWorkClass {
    if has_label_or_title(candidate, "tracker-only")
        || has_label_or_title(candidate, "beads")
        || has_label_or_title(candidate, "tracker")
    {
        return UnsafeClaimAlternateWorkClass::TrackerOnly;
    }
    if all_paths_match(candidate, |path| path.starts_with("docs")) {
        return UnsafeClaimAlternateWorkClass::DocsOnly;
    }
    if has_label_or_title(candidate, "contract")
        || has_label_or_title(candidate, "schema")
        || has_label_or_title(candidate, "design")
        || has_label_or_title(candidate, "adr")
    {
        return UnsafeClaimAlternateWorkClass::ContractDesign;
    }
    if has_label_or_title(candidate, "fixture")
        || has_label_or_title(candidate, "golden")
        || all_paths_match(candidate, |path| path.starts_with("tests/fixtures"))
    {
        return UnsafeClaimAlternateWorkClass::FixtureOnly;
    }
    if has_label_or_title(candidate, "shell-only")
        || has_label_or_title(candidate, "no-cargo")
        || all_paths_match(candidate, |path| path.starts_with("scripts"))
    {
        return UnsafeClaimAlternateWorkClass::ShellOnlyNoCargo;
    }
    if candidate
        .path_families
        .iter()
        .map(|path| normalized_path_family(path))
        .any(|path| path.starts_with("src") || path.starts_with("tests"))
    {
        return UnsafeClaimAlternateWorkClass::RustSource;
    }
    UnsafeClaimAlternateWorkClass::Unknown
}

fn freshness_rank(freshness: &str) -> usize {
    match freshness {
        "fresh" => 0,
        "stale" => 2,
        "unknown" => 3,
        _ => 1,
    }
}

fn alternate_state(
    candidate: &UnsafeClaimAlternateCandidateFacts,
    unsafe_path_families: &[String],
) -> (UnsafeClaimAlternateCandidateState, Option<String>) {
    if candidate.candidate_id.trim().is_empty() {
        return (UnsafeClaimAlternateCandidateState::NotFound, None);
    }
    if candidate.status != "open"
        || candidate
            .assignee
            .as_ref()
            .is_some_and(|assignee| !assignee.trim().is_empty())
        || candidate.issue_type == "epic"
    {
        return (UnsafeClaimAlternateCandidateState::BlockedOrOwned, None);
    }
    if let Some(overlap) = candidate_overlaps_unsafe_surface(candidate, unsafe_path_families) {
        return (
            UnsafeClaimAlternateCandidateState::BlockedOrOwned,
            Some(overlap),
        );
    }
    if candidate.gate_safe_to_claim == Some(true) && candidate.gate_claim_command_action_present {
        return (UnsafeClaimAlternateCandidateState::FreshSafeToClaim, None);
    }
    if candidate.gate_safe_to_claim == Some(false) {
        return (UnsafeClaimAlternateCandidateState::ScannedAndUnsafe, None);
    }
    (
        UnsafeClaimAlternateCandidateState::PlausibleButRequiresGate,
        None,
    )
}

fn push_delta(deltas: &mut Vec<String>, value: impl Into<String>) {
    if deltas.len() >= MAX_CANDIDATE_DELTAS {
        return;
    }
    let value = value.into();
    if !value.trim().is_empty() && !deltas.contains(&value) {
        deltas.push(value);
    }
}

fn bead_show_action(candidate_id: &str) -> UnsafeClaimReadOnlyCommandAction {
    UnsafeClaimReadOnlyCommandAction {
        command_id: format!("bead_show_candidate:{candidate_id}"),
        display_command: format!("br show {candidate_id} --json"),
        argv: vec![
            "br".to_owned(),
            "show".to_owned(),
            candidate_id.to_owned(),
            "--json".to_owned(),
        ],
        shell_required: false,
        copy_safety: "safe_structured_argv",
        mutates_state: false,
        required_substrate: "beads",
        when: "inspect_alternate_before_claim_gate".to_owned(),
        rationale: "Inspect the alternate Bead without claiming it.".to_owned(),
    }
}

fn claim_gate_retry_action(candidate_id: &str) -> UnsafeClaimReadOnlyCommandAction {
    UnsafeClaimReadOnlyCommandAction {
        command_id: format!("claim_gate_retry:{candidate_id}"),
        display_command: format!(
            "ee swarm work-packet --workspace . --include-rch --claim-gate --candidate {candidate_id} --json"
        ),
        argv: vec![
            "ee".to_owned(),
            "swarm".to_owned(),
            "work-packet".to_owned(),
            "--workspace".to_owned(),
            ".".to_owned(),
            "--include-rch".to_owned(),
            "--claim-gate".to_owned(),
            "--candidate".to_owned(),
            candidate_id.to_owned(),
            "--json".to_owned(),
        ],
        shell_required: false,
        copy_safety: "safe_structured_argv",
        mutates_state: false,
        required_substrate: "ee",
        when: "after_inspection_before_any_claim".to_owned(),
        rationale: "Generate a fresh read-only claim-gate verdict for this alternate.".to_owned(),
    }
}

fn recommended_action_for(candidates: &[UnsafeClaimAlternateRecommendation]) -> &'static str {
    if candidates.iter().any(|candidate| {
        candidate.candidate_state == UnsafeClaimAlternateCandidateState::FreshSafeToClaim
    }) {
        "inspect_fresh_safe_candidate"
    } else if candidates.iter().any(|candidate| {
        candidate.candidate_state == UnsafeClaimAlternateCandidateState::PlausibleButRequiresGate
    }) {
        "gate_plausible_alternate"
    } else {
        "stop_or_coordinate"
    }
}

/// Recommend alternate candidates from already-collected facts. The output is
/// bounded, deterministic, and advisory-only; every recommended candidate
/// either needs its own fresh claim gate or points back to an already-fresh
/// safe gate owned by the source gate, never to a newly invented claim command.
#[must_use]
pub fn recommend_unsafe_claim_alternates(
    input: &UnsafeClaimAlternatePlannerInput,
) -> UnsafeClaimAlternateRecommendationPlan {
    let mut candidates = Vec::new();
    for candidate in &input.candidates {
        let (state, overlap) = alternate_state(candidate, &input.unsafe_path_families);
        let work_class = classify_alternate_work(candidate);
        let priority = candidate.priority.unwrap_or(2);
        let mut reason_group_refs = candidate.reason_group_refs.clone();
        let mut deltas = candidate.candidate_specific_deltas.clone();
        let needs_fresh_claim_gate =
            state == UnsafeClaimAlternateCandidateState::PlausibleButRequiresGate;

        if reason_group_refs.is_empty() {
            match state {
                UnsafeClaimAlternateCandidateState::BlockedOrOwned => {
                    reason_group_refs.push("rg-reservation-conflict".to_owned());
                }
                UnsafeClaimAlternateCandidateState::ScannedAndUnsafe => {
                    reason_group_refs.push("rg-recommendation-mismatch".to_owned());
                }
                UnsafeClaimAlternateCandidateState::Unknown => {
                    reason_group_refs.push("rg-unknown".to_owned());
                }
                _ => {}
            }
        }

        push_delta(&mut deltas, format!("state:{}", state.as_str()));
        push_delta(&mut deltas, format!("work_class:{}", work_class.as_str()));
        push_delta(
            &mut deltas,
            format!("tracker_health:{}", input.tracker_health),
        );
        push_delta(
            &mut deltas,
            format!("agent_mail_status:{}", input.agent_mail_status),
        );
        push_delta(
            &mut deltas,
            format!("source_freshness:{}", input.source_freshness),
        );
        push_delta(
            &mut deltas,
            format!("evidence_freshness:{}", candidate.evidence_freshness),
        );
        if needs_fresh_claim_gate {
            push_delta(&mut deltas, "needs_fresh_claim_gate");
        }
        if let Some(overlap) = overlap {
            push_delta(&mut deltas, format!("overlaps_unsafe_surface:{overlap}"));
        }

        let mut advisory_notes = Vec::new();
        advisory_notes.push("alternate_recommendation_is_advisory_only".to_owned());
        advisory_notes.push(format!(
            "source_freshness={}, tracker_health={}, agent_mail_status={}",
            input.source_freshness, input.tracker_health, input.agent_mail_status
        ));
        if state == UnsafeClaimAlternateCandidateState::FreshSafeToClaim {
            advisory_notes.push(
                "fresh gate exists; use the source gate directly for any claim command".to_owned(),
            );
        } else if needs_fresh_claim_gate {
            advisory_notes.push(
                "run a fresh claim gate for this alternate before any Beads mutation".to_owned(),
            );
        }

        let mut next_command_actions = vec![bead_show_action(&candidate.candidate_id)];
        if needs_fresh_claim_gate {
            next_command_actions.push(claim_gate_retry_action(&candidate.candidate_id));
        }

        candidates.push(UnsafeClaimAlternateRecommendation {
            rank: 0,
            candidate_id: candidate.candidate_id.clone(),
            candidate_state: state,
            work_class,
            gate_verdict: candidate
                .gate_verdict
                .clone()
                .unwrap_or_else(|| "not_scanned".to_owned()),
            safe_to_claim: candidate.gate_safe_to_claim.unwrap_or(false),
            priority,
            score: candidate.score,
            reason_group_refs,
            candidate_specific_deltas: deltas,
            needs_fresh_claim_gate,
            may_emit_claim_command: false,
            advisory_notes,
            next_command_actions,
        });
    }

    candidates.sort_by(|left, right| {
        let left_key = (
            left.candidate_state.rank(),
            if input.source_authority_degraded {
                left.work_class.degraded_authority_rank()
            } else {
                0
            },
            left.priority,
            std::cmp::Reverse(left.score),
            freshness_rank(
                left.candidate_specific_deltas
                    .iter()
                    .find_map(|delta| delta.strip_prefix("evidence_freshness:"))
                    .unwrap_or("unknown"),
            ),
            left.candidate_id.as_str(),
        );
        let right_key = (
            right.candidate_state.rank(),
            if input.source_authority_degraded {
                right.work_class.degraded_authority_rank()
            } else {
                0
            },
            right.priority,
            std::cmp::Reverse(right.score),
            freshness_rank(
                right
                    .candidate_specific_deltas
                    .iter()
                    .find_map(|delta| delta.strip_prefix("evidence_freshness:"))
                    .unwrap_or("unknown"),
            ),
            right.candidate_id.as_str(),
        );
        left_key.cmp(&right_key)
    });

    candidates.truncate(MAX_ALTERNATE_RECOMMENDATIONS);
    for (index, candidate) in candidates.iter_mut().enumerate() {
        candidate.rank = index + 1;
    }

    let next_command_actions = candidates
        .first()
        .map(|candidate| candidate.next_command_actions.clone())
        .unwrap_or_default();
    let recommended_action = recommended_action_for(&candidates).to_owned();

    UnsafeClaimAlternateRecommendationPlan {
        recommended_action,
        source_authority_degraded: input.source_authority_degraded,
        candidates,
        next_command_actions,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// bd-1n3x1.16 — ee.swarm.unsafe_claim_plan.v1 envelope assembler.
//
// Turns the read-only evidence of an unsafe ee.swarm.work_packet.claim_gate.v1
// result into the full companion plan contract
// (`docs/schemas/swarm/ee.swarm.unsafe_claim_plan.v1.json`). Pure and
// non-mutating: it only reshapes already-gathered gate evidence and never
// re-runs git, Beads, BV, RCH, or Agent Mail. Deterministic: the same gate
// evidence (ignoring wall-clock `generatedAt`) yields the same `planId` and
// `provenanceHash`.
// ───────────────────────────────────────────────────────────────────────────

/// Schema id for the companion unsafe-claim plan projection.
pub const UNSAFE_CLAIM_PLAN_SCHEMA_V1: &str = "ee.swarm.unsafe_claim_plan.v1";

const UNSAFE_CLAIM_PLAN_REDACTION_STATUS: &str =
    "counts_ids_statuses_path_patterns_command_templates_no_mail_body_no_file_content";

/// Echo of the source claim gate (the unsafe ee.swarm.work_packet.claim_gate.v1
/// fields the plan preserves verbatim).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimPlanSourceGate {
    pub schema: &'static str,
    pub gate_id: String,
    pub packet_id: String,
    pub requested_candidate_id: Option<String>,
    pub selected_candidate_id: Option<String>,
    pub verdict: String,
    /// Always false; the plan only exists for unsafe gates.
    pub safe_to_claim: bool,
    pub recommended_action: String,
    pub recommended_safe_to_claim: Option<bool>,
    /// Always null in the unsafe projection.
    pub claim_command_action: Option<UnsafeClaimReadOnlyCommandAction>,
    pub unsafe_reasons: Vec<String>,
    pub stale_reasons: Vec<String>,
    pub degraded_codes: Vec<String>,
    pub source_refs: Vec<String>,
    pub next_command_actions: Vec<UnsafeClaimReadOnlyCommandAction>,
}

/// Enriched reason group matching the schema `reasonGroup` shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimPlanReasonGroup {
    pub group_id: String,
    pub category: &'static str,
    pub severity: &'static str,
    pub reason_codes: Vec<String>,
    pub raw_reason_indexes: Vec<usize>,
    pub source_fields: Vec<&'static str>,
    pub evidence_source_ids: Vec<String>,
    pub candidate_coverage: Vec<&'static str>,
    pub bounded_preview: String,
    pub preserves_unknown: bool,
}

/// One candidate's plan slice (schema `candidatePlan`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimPlanCandidatePlan {
    pub candidate_id: Option<String>,
    pub candidate_state: &'static str,
    pub gate_verdict: String,
    pub safe_to_claim: bool,
    pub reason_group_refs: Vec<String>,
    pub candidate_specific_deltas: Vec<String>,
    pub needs_fresh_claim_gate: bool,
    /// Always false in the unsafe projection.
    pub may_emit_claim_command: bool,
    pub evidence_source_ids: Vec<String>,
}

/// Bounded body template for a `comment_template` action (schema `bodyTemplate`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimPlanBodyTemplate {
    pub template_kind: &'static str,
    pub title: String,
    pub body_preview: String,
    pub truncated: bool,
}

/// Enriched advisory action matching the schema `plannerAction` shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimPlanAction {
    pub action_id: String,
    pub kind: &'static str,
    pub mutates_state: bool,
    pub advisory_only: bool,
    pub required_substrate: &'static str,
    pub copy_safety: &'static str,
    pub when: &'static str,
    pub rationale: String,
    pub reason_group_refs: Vec<String>,
    pub evidence_source_ids: Vec<String>,
    pub command_action: Option<UnsafeClaimReadOnlyCommandAction>,
    pub body_template: Option<UnsafeClaimPlanBodyTemplate>,
}

/// One evidence source descriptor (schema `evidenceSource`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimPlanEvidenceSource {
    pub source_id: String,
    pub source_kind: &'static str,
    pub source_ref: String,
    pub freshness_state: &'static str,
    pub authoritative: bool,
}

/// Deterministic ordering contract block (all fixed const strings).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimPlanOrdering {
    pub reason_groups: &'static str,
    pub candidate_plans: &'static str,
    pub planner_actions: &'static str,
    pub evidence_sources: &'static str,
}

/// Non-mutation policy block (all fixed const booleans).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimPlanNonMutationPolicy {
    pub advisory_only: bool,
    pub claims_beads: bool,
    pub reserves_files: bool,
    pub sends_agent_mail: bool,
    pub runs_cargo: bool,
    pub stages_git: bool,
    pub deletes_files: bool,
}

/// One degradation entry (schema `degradation`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimPlanDegradation {
    pub code: String,
    pub severity: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// The full `ee.swarm.unsafe_claim_plan.v1` envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsafeClaimPlan {
    pub schema: &'static str,
    pub plan_id: String,
    pub generated_at: String,
    pub workspace: String,
    pub redaction_status: &'static str,
    pub ordering: UnsafeClaimPlanOrdering,
    pub source_gate: UnsafeClaimPlanSourceGate,
    pub reason_groups: Vec<UnsafeClaimPlanReasonGroup>,
    pub candidate_plans: Vec<UnsafeClaimPlanCandidatePlan>,
    pub planner_actions: Vec<UnsafeClaimPlanAction>,
    pub next_command_actions: Vec<UnsafeClaimReadOnlyCommandAction>,
    pub evidence_sources: Vec<UnsafeClaimPlanEvidenceSource>,
    pub non_mutation_policy: UnsafeClaimPlanNonMutationPolicy,
    pub degraded: Vec<UnsafeClaimPlanDegradation>,
    pub provenance_hash: String,
}

/// Pure input for [`build_unsafe_claim_plan`]. The caller fills these from a
/// freshly built (unsafe) claim gate; nothing here re-runs a source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsafeClaimPlanInput {
    /// Wall-clock RFC 3339 timestamp; intentionally excluded from the content
    /// hash so identical evidence is byte-stable across runs.
    pub generated_at: String,
    pub workspace: String,
    pub gate_id: String,
    pub packet_id: String,
    pub requested_candidate_id: Option<String>,
    pub selected_candidate_id: Option<String>,
    pub verdict: String,
    pub recommended_action: String,
    pub recommended_safe_to_claim: Option<bool>,
    pub unsafe_reasons: Vec<String>,
    pub stale_reasons: Vec<String>,
    pub degraded_codes: Vec<String>,
    pub source_refs: Vec<String>,
    pub next_command_actions: Vec<UnsafeClaimReadOnlyCommandAction>,
}

/// The evidence `sourceKind` that owns a reason category (schema enum).
const fn evidence_source_kind(category: UnsafeClaimReasonCategory) -> &'static str {
    use UnsafeClaimReasonCategory as UnsafeCategory;
    match category {
        UnsafeCategory::TrackerAuthority | UnsafeCategory::RecommendationMismatch => "beads",
        UnsafeCategory::AgentMailReadiness | UnsafeCategory::ReservationConflict => "agent_mail",
        UnsafeCategory::SourceOverlap => "workspace_hygiene",
        UnsafeCategory::DirtyCheckout => "git",
        UnsafeCategory::RchProofAdmission => "rch",
        UnsafeCategory::InstalledBinaryFreshness => "installed_binary",
        UnsafeCategory::BvStaleness => "bv",
        UnsafeCategory::MemorySourceDrift => "memory_drift",
        UnsafeCategory::ResourceAdmission => "resource_admission",
        UnsafeCategory::ActionSuppression | UnsafeCategory::Unknown => "source_authority",
    }
}

/// `rg-<kebab-category>` group id (schema pattern `^rg-[a-z0-9-]+$`).
fn reason_group_id(category: UnsafeClaimReasonCategory) -> String {
    format!("rg-{}", category.as_str().replace('_', "-"))
}

/// `src-<kebab-kind>` evidence id (schema pattern `^src-[a-z0-9-]+$`).
fn evidence_source_id(kind: &str) -> String {
    format!("src-{}", kind.replace('_', "-"))
}

/// Freshness/authority for an evidence source: locally observed git/workspace
/// signals are authoritative; degraded coordination sources are advisory.
fn evidence_freshness(kind: &str) -> (&'static str, bool) {
    match kind {
        "git" | "workspace_hygiene" => ("fresh", true),
        _ => ("unknown", false),
    }
}

/// The `commandSubstrate` an action's next step runs on.
const fn action_substrate(kind: UnsafeClaimActionKind) -> &'static str {
    use UnsafeClaimActionKind as UnsafeKind;
    match kind {
        UnsafeKind::Inspect | UnsafeKind::RetryWithSnapshot => "ee",
        UnsafeKind::CommentTemplate | UnsafeKind::WaitOrCoordinate => "human",
        UnsafeKind::DecomposeCandidate | UnsafeKind::AlternateCandidate => "beads",
        UnsafeKind::Stop => "none",
    }
}

/// The bounded `when` phrase for an action kind.
const fn action_when(kind: UnsafeClaimActionKind) -> &'static str {
    use UnsafeClaimActionKind as UnsafeKind;
    match kind {
        UnsafeKind::Inspect => "before_any_mutation",
        UnsafeKind::CommentTemplate => "if_operator_chooses_to_coordinate",
        UnsafeKind::DecomposeCandidate => "if_surface_is_separable",
        UnsafeKind::AlternateCandidate => "when_choosing_other_work",
        UnsafeKind::RetryWithSnapshot => "after_fresh_redacted_snapshot",
        UnsafeKind::WaitOrCoordinate => "while_shared_substrate_in_motion",
        UnsafeKind::Stop => "if_policy_or_suppression_signal_present",
    }
}

/// Map a candidate's gate verdict to its schema candidate state.
fn unsafe_claim_candidate_state(verdict: &str, unsafe_reasons: &[String]) -> &'static str {
    let head = verdict.split(':').next().unwrap_or(verdict);
    if head.contains("not_found")
        || unsafe_reasons
            .iter()
            .any(|reason| reason.starts_with("candidate_not_found"))
    {
        "not_found"
    } else if head.contains("blocked")
        || unsafe_reasons.iter().any(|reason| {
            reason.starts_with("blocked_by") || reason.starts_with("candidate_assigned_to")
        })
    {
        "blocked_or_owned"
    } else {
        "scanned_and_unsafe"
    }
}

/// Deterministic evidence source ids for an action's category refs.
fn action_evidence_source_ids(category_refs: &[&'static str]) -> Vec<String> {
    let mut ids: Vec<String> = category_refs
        .iter()
        .filter_map(|reference| UnsafeClaimReasonCategory::from_contract_str(reference))
        .map(|category| evidence_source_id(evidence_source_kind(category)))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The read-only `ee swarm work-packet --claim-gate` retry command.
fn unsafe_claim_retry_command(candidate: Option<&str>) -> UnsafeClaimReadOnlyCommandAction {
    let mut argv = vec![
        "ee".to_owned(),
        "swarm".to_owned(),
        "work-packet".to_owned(),
        "--claim-gate".to_owned(),
    ];
    if let Some(id) = candidate {
        argv.push("--candidate".to_owned());
        argv.push(id.to_owned());
    }
    argv.push("--json".to_owned());
    UnsafeClaimReadOnlyCommandAction {
        command_id: "claim_gate_retry_with_snapshot".to_owned(),
        display_command: argv.join(" "),
        argv,
        shell_required: false,
        copy_safety: "safe_structured_argv",
        mutates_state: false,
        required_substrate: "ee",
        when: "after_fresh_redacted_snapshot".to_owned(),
        rationale: "Regenerate a read-only gate verdict after fresh coordination evidence; this \
                    does not claim work."
            .to_owned(),
    }
}

/// Build the full `ee.swarm.unsafe_claim_plan.v1` envelope from an unsafe
/// claim gate's read-only evidence. Advisory only; never mutates state.
#[must_use]
pub fn build_unsafe_claim_plan(input: &UnsafeClaimPlanInput) -> UnsafeClaimPlan {
    let classification =
        classify_unsafe_claim_evidence(&input.unsafe_reasons, &input.degraded_codes);
    let unsafe_len = input.unsafe_reasons.len();
    let requested = input.requested_candidate_id.is_some();

    // Reason groups already arrive in reasonCategory enum order from the
    // classifier; enrich each with schema-only fields.
    let reason_groups: Vec<UnsafeClaimPlanReasonGroup> = classification
        .reason_groups
        .iter()
        .map(|group| {
            let kind = evidence_source_kind(group.category);
            let mut source_fields: Vec<&'static str> = group
                .raw_reason_indexes
                .iter()
                .map(|index| {
                    if *index < unsafe_len {
                        "sourceGate.unsafeReasons"
                    } else {
                        "sourceGate.degradedCodes"
                    }
                })
                .collect();
            source_fields.dedup();
            let candidate_coverage = if group.category == UnsafeClaimReasonCategory::Unknown {
                vec!["unknown"]
            } else if requested
                && matches!(
                    group.category,
                    UnsafeClaimReasonCategory::SourceOverlap
                        | UnsafeClaimReasonCategory::ReservationConflict
                        | UnsafeClaimReasonCategory::RecommendationMismatch
                )
            {
                vec!["requested_candidate"]
            } else {
                vec!["all_candidates"]
            };
            UnsafeClaimPlanReasonGroup {
                group_id: reason_group_id(group.category),
                category: group.category.as_str(),
                severity: group.severity,
                reason_codes: group.reason_codes.clone(),
                raw_reason_indexes: group.raw_reason_indexes.clone(),
                source_fields,
                evidence_source_ids: vec![evidence_source_id(kind)],
                candidate_coverage,
                bounded_preview: group.bounded_preview.clone(),
                preserves_unknown: group.category == UnsafeClaimReasonCategory::Unknown,
            }
        })
        .collect();

    // One evidence source per distinct kind, sorted by sourceId.
    let mut source_kinds: Vec<&'static str> = classification
        .reason_groups
        .iter()
        .map(|group| evidence_source_kind(group.category))
        .collect();
    source_kinds.sort_unstable();
    source_kinds.dedup();
    let mut evidence_sources: Vec<UnsafeClaimPlanEvidenceSource> = source_kinds
        .iter()
        .map(|kind| {
            let (freshness, authoritative) = evidence_freshness(kind);
            UnsafeClaimPlanEvidenceSource {
                source_id: evidence_source_id(kind),
                source_kind: kind,
                source_ref: format!("{}://gate-{}", kind.replace('_', "-"), input.gate_id),
                freshness_state: freshness,
                authoritative,
            }
        })
        .collect();
    evidence_sources.sort_by(|left, right| left.source_id.cmp(&right.source_id));

    let all_group_refs: Vec<String> = reason_groups
        .iter()
        .map(|group| group.group_id.clone())
        .collect();
    let all_evidence_ids: Vec<String> = evidence_sources
        .iter()
        .map(|source| source.source_id.clone())
        .collect();

    // Planner actions (already in plannerActionKind order from the ranker).
    let planner_actions: Vec<UnsafeClaimPlanAction> = classification
        .planner_actions
        .iter()
        .map(|action| {
            let group_refs: Vec<String> = action
                .reason_group_refs
                .iter()
                .map(|reference| format!("rg-{}", reference.replace('_', "-")))
                .collect();
            let command_action = (action.kind == UnsafeClaimActionKind::RetryWithSnapshot)
                .then(|| unsafe_claim_retry_command(input.requested_candidate_id.as_deref()));
            let body_template =
                (action.kind == UnsafeClaimActionKind::CommentTemplate).then(|| {
                    UnsafeClaimPlanBodyTemplate {
                        template_kind: "beads_comment",
                        title: "Unsafe claim gate coordination needed".to_owned(),
                        body_preview: action.rationale.clone(),
                        truncated: false,
                    }
                });
            let copy_safety = if command_action.is_some() {
                "safe_structured_argv"
            } else {
                "display_only"
            };
            UnsafeClaimPlanAction {
                action_id: format!("act-{}", action.kind.as_str().replace('_', "-")),
                kind: action.kind.as_str(),
                mutates_state: false,
                advisory_only: true,
                required_substrate: action_substrate(action.kind),
                copy_safety,
                when: action_when(action.kind),
                rationale: action.rationale.clone(),
                reason_group_refs: group_refs,
                evidence_source_ids: action_evidence_source_ids(&action.reason_group_refs),
                command_action,
                body_template,
            }
        })
        .collect();

    // Candidate plan (schema requires at least one).
    let candidate_id = input
        .requested_candidate_id
        .clone()
        .or_else(|| input.selected_candidate_id.clone());
    let mut plan_next_actions: Vec<UnsafeClaimReadOnlyCommandAction> = Vec::new();
    if let Some(id) = candidate_id.as_deref() {
        plan_next_actions.push(UnsafeClaimReadOnlyCommandAction {
            command_id: "bead_show_candidate_read_only".to_owned(),
            display_command: format!("br show {id} --json"),
            argv: vec![
                "br".to_owned(),
                "show".to_owned(),
                id.to_owned(),
                "--json".to_owned(),
            ],
            shell_required: false,
            copy_safety: "safe_structured_argv",
            mutates_state: false,
            required_substrate: "beads",
            when: "before_choosing_alternate_work".to_owned(),
            rationale: "Inspect the candidate Bead without mutating tracker state.".to_owned(),
        });
    }
    let mut candidate_specific_deltas = vec!["claim_command_suppressed".to_owned()];
    if requested {
        candidate_specific_deltas.push("requested_candidate_preserved".to_owned());
    }
    candidate_specific_deltas.truncate(MAX_CANDIDATE_DELTAS);
    let candidate_plans = vec![UnsafeClaimPlanCandidatePlan {
        candidate_state: unsafe_claim_candidate_state(&input.verdict, &input.unsafe_reasons),
        candidate_id,
        gate_verdict: input.verdict.clone(),
        safe_to_claim: false,
        reason_group_refs: all_group_refs,
        candidate_specific_deltas,
        needs_fresh_claim_gate: true,
        may_emit_claim_command: false,
        evidence_source_ids: all_evidence_ids,
    }];

    let source_gate = UnsafeClaimPlanSourceGate {
        schema: "ee.swarm.work_packet.claim_gate.v1",
        gate_id: input.gate_id.clone(),
        packet_id: input.packet_id.clone(),
        requested_candidate_id: input.requested_candidate_id.clone(),
        selected_candidate_id: input.selected_candidate_id.clone(),
        verdict: input.verdict.clone(),
        safe_to_claim: false,
        recommended_action: input.recommended_action.clone(),
        recommended_safe_to_claim: input.recommended_safe_to_claim,
        claim_command_action: None,
        unsafe_reasons: input.unsafe_reasons.clone(),
        stale_reasons: input.stale_reasons.clone(),
        degraded_codes: input.degraded_codes.clone(),
        source_refs: input.source_refs.clone(),
        next_command_actions: input.next_command_actions.clone(),
    };

    let mut plan = UnsafeClaimPlan {
        schema: UNSAFE_CLAIM_PLAN_SCHEMA_V1,
        plan_id: String::new(),
        generated_at: input.generated_at.clone(),
        workspace: input.workspace.clone(),
        redaction_status: UNSAFE_CLAIM_PLAN_REDACTION_STATUS,
        ordering: UnsafeClaimPlanOrdering {
            reason_groups: "reasonCategory enum order, then groupId ascending byte order",
            candidate_plans: "candidateId ascending byte order",
            planner_actions: "plannerActionKind enum order, then actionId ascending byte order",
            evidence_sources: "sourceId ascending byte order",
        },
        source_gate,
        reason_groups,
        candidate_plans,
        planner_actions,
        next_command_actions: plan_next_actions,
        evidence_sources,
        non_mutation_policy: UnsafeClaimPlanNonMutationPolicy {
            advisory_only: true,
            claims_beads: false,
            reserves_files: false,
            sends_agent_mail: false,
            runs_cargo: false,
            stages_git: false,
            deletes_files: false,
        },
        degraded: Vec::new(),
        provenance_hash: String::new(),
    };

    // Deterministic content hash over everything except the wall-clock and the
    // self-referential id fields.
    let digest = serde_json::to_value(&plan)
        .ok()
        .and_then(|mut value| {
            if let Some(object) = value.as_object_mut() {
                object.remove("planId");
                object.remove("generatedAt");
                object.remove("provenanceHash");
            }
            serde_json::to_vec(&value).ok()
        })
        .map_or_else(
            || "0".repeat(64),
            |bytes| blake3::hash(&bytes).to_hex().to_string(),
        );
    plan.plan_id = format!("unsafe_claim_plan_{}", &digest[..24]);
    plan.provenance_hash = format!("blake3:{digest}");
    plan
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

    fn has_action(
        classification: &UnsafeClaimClassification,
        kind: UnsafeClaimActionKind,
        reason_ref: &str,
    ) -> bool {
        classification
            .planner_actions
            .iter()
            .any(|action| action.kind == kind && action.reason_group_refs.contains(&reason_ref))
    }

    #[test]
    fn enum_string_orders_match_unsafe_claim_plan_schema() -> TestResult {
        let reason_categories: Vec<_> = UnsafeClaimReasonCategory::ALL
            .iter()
            .map(|category| category.as_str())
            .collect();
        ensure(
            reason_categories
                == vec![
                    "tracker_authority",
                    "agent_mail_readiness",
                    "source_overlap",
                    "dirty_checkout",
                    "rch_proof_admission",
                    "installed_binary_freshness",
                    "reservation_conflict",
                    "bv_staleness",
                    "recommendation_mismatch",
                    "memory_source_drift",
                    "resource_admission",
                    "action_suppression",
                    "unknown",
                ],
            format!("reason category schema order drifted: {reason_categories:?}"),
        )?;

        let action_kinds = [
            UnsafeClaimActionKind::Inspect,
            UnsafeClaimActionKind::CommentTemplate,
            UnsafeClaimActionKind::DecomposeCandidate,
            UnsafeClaimActionKind::AlternateCandidate,
            UnsafeClaimActionKind::RetryWithSnapshot,
            UnsafeClaimActionKind::WaitOrCoordinate,
            UnsafeClaimActionKind::Stop,
        ]
        .into_iter()
        .map(UnsafeClaimActionKind::as_str)
        .collect::<Vec<_>>();
        ensure(
            action_kinds
                == vec![
                    "inspect",
                    "comment_template",
                    "decompose_candidate",
                    "alternate_candidate",
                    "retry_with_snapshot",
                    "wait_or_coordinate",
                    "stop",
                ],
            format!("action kind schema order drifted: {action_kinds:?}"),
        )
    }

    #[test]
    fn fixture_category_severities_match_unsafe_claim_plan_schema_example() -> TestResult {
        for (category, expected) in [
            (UnsafeClaimReasonCategory::TrackerAuthority, "high"),
            (UnsafeClaimReasonCategory::AgentMailReadiness, "medium"),
            (UnsafeClaimReasonCategory::SourceOverlap, "medium"),
            (UnsafeClaimReasonCategory::DirtyCheckout, "warning"),
            (UnsafeClaimReasonCategory::RchProofAdmission, "medium"),
            (UnsafeClaimReasonCategory::BvStaleness, "warning"),
            (UnsafeClaimReasonCategory::Unknown, "warning"),
        ] {
            let got = category.severity();
            ensure(
                got == expected,
                format!(
                    "{} severity drifted: expected {expected}, got {got}",
                    category.as_str()
                ),
            )?;
        }
        Ok(())
    }

    #[test]
    fn category_rank_matches_preallocated_group_order() -> TestResult {
        for (index, category) in UnsafeClaimReasonCategory::ALL.iter().enumerate() {
            ensure(
                category.rank() == index,
                format!(
                    "{} rank drifted: expected {index}, got {}",
                    category.as_str(),
                    category.rank()
                ),
            )?;
        }
        Ok(())
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
    fn preinspection_failures_stay_resource_admission_without_hiding_memory_drift() -> TestResult {
        let classification = classify_unsafe_claim_evidence(
            &strings(&[
                "memory_drift_lock_contention",
                "memory_drift_report_unavailable",
                "memory_drift_source_unverifiable",
                "source_authority_memory_drift_lock_contention",
                "source_authority_memory_drift_report_unavailable",
            ]),
            &strings(&[
                "memory_drift_lock_contention",
                "memory_drift_report_unavailable",
            ]),
        );

        let resource_group = classification
            .reason_groups
            .iter()
            .find(|group| group.category == UnsafeClaimReasonCategory::ResourceAdmission)
            .ok_or("lock contention must produce resource_admission group")?;
        ensure(
            resource_group
                .reason_codes
                .iter()
                .filter(|code| code.as_str() == "memory_drift_lock_contention")
                .count()
                == 2,
            format!(
                "resource_admission must preserve raw lock-contention reason and degraded code, got {:?}",
                resource_group.reason_codes
            ),
        )?;
        ensure(
            [
                "source_authority_memory_drift_lock_contention",
                "source_authority_memory_drift_report_unavailable",
            ]
            .iter()
            .all(|expected| {
                resource_group
                    .reason_codes
                    .iter()
                    .any(|code| code.as_str() == *expected)
            }),
            format!(
                "source-authority pre-inspection failures must remain resource admission, got {:?}",
                resource_group.reason_codes
            ),
        )?;
        ensure(
            resource_group
                .reason_codes
                .iter()
                .filter(|code| code.as_str() == "memory_drift_report_unavailable")
                .count()
                == 2,
            format!(
                "resource_admission must preserve raw report-unavailable reason and degraded code, got {:?}",
                resource_group.reason_codes
            ),
        )?;

        let drift_group = classification
            .reason_groups
            .iter()
            .find(|group| group.category == UnsafeClaimReasonCategory::MemorySourceDrift)
            .ok_or("ordinary memory drift must keep memory_source_drift group")?;
        ensure(
            drift_group
                .reason_codes
                .iter()
                .any(|code| code == "memory_drift_source_unverifiable"),
            format!(
                "memory_source_drift must preserve ordinary drift evidence, got {:?}",
                drift_group.reason_codes
            ),
        )?;
        ensure(
            has_action(
                &classification,
                UnsafeClaimActionKind::WaitOrCoordinate,
                UnsafeClaimReasonCategory::ResourceAdmission.as_str(),
            ),
            "pre-inspection resource-admission failures must recommend wait_or_coordinate",
        )?;
        ensure(
            classification
                .planner_actions
                .iter()
                .all(|action| action.advisory_only && !action.mutates_state),
            "pre-inspection memory-drift planner actions must stay advisory-only",
        )
    }

    #[test]
    fn action_families_rank_deterministically() -> TestResult {
        let reasons = strings(&[
            "reservation_evidence_not_authoritative",
            "agent_mail_unavailable",
            "candidate_not_found:bd-abc",
            "dirty_checkout_path_count:3",
            "memory_drift_source_unverifiable",
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
                    UnsafeClaimActionKind::Inspect,
                    UnsafeClaimActionKind::DecomposeCandidate,
                    UnsafeClaimActionKind::AlternateCandidate,
                    UnsafeClaimActionKind::RetryWithSnapshot,
                    UnsafeClaimActionKind::WaitOrCoordinate,
                    UnsafeClaimActionKind::Stop,
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
    fn bounded_reason_respects_byte_cap_and_utf8_boundaries() -> TestResult {
        let long_ascii = format!("rch_{}", "x".repeat(MAX_RAW_REASON_LEN + 10));
        let bounded_ascii = bounded_reason(&long_ascii);
        ensure(
            bounded_ascii.len() == MAX_RAW_REASON_LEN,
            format!(
                "bounded ASCII reason must be exactly {MAX_RAW_REASON_LEN} bytes, got {}",
                bounded_ascii.len()
            ),
        )?;
        ensure(
            bounded_ascii.ends_with(TRUNCATED_REASON_MARKER),
            "bounded ASCII reason must disclose truncation",
        )?;

        let long_utf8 = format!("memory_probe_{}", "é".repeat(MAX_RAW_REASON_LEN));
        let bounded_utf8 = bounded_reason(&long_utf8);
        ensure(
            bounded_utf8.len() <= MAX_RAW_REASON_LEN,
            format!(
                "bounded UTF-8 reason must stay within {MAX_RAW_REASON_LEN} bytes, got {}",
                bounded_utf8.len()
            ),
        )?;
        ensure(
            bounded_utf8.ends_with(TRUNCATED_REASON_MARKER),
            "bounded UTF-8 reason must disclose truncation",
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
                "tracker_not_authoritative",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "beads_requires_candidate_downgrade",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "beads_db_jsonl_count_mismatch",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "beads_unavailable",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "beads_command_timeout",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "beads_no_output",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "beads_metadata_only_stale",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "beads_tracker_metadata_drift",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "tracker_authority_degraded",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "actionable_queue_unavailable",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "actionable_queue_timed_out",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "actionable_queue_stale_fallback",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "source_authority_actionable_queue_timed_out",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "source_authority_beads_stale_fallback",
                UnsafeClaimReasonCategory::TrackerAuthority,
            ),
            (
                "agent_mail_recovery_corrupt",
                UnsafeClaimReasonCategory::AgentMailReadiness,
            ),
            (
                "source_authority_agent_mail_corrupt_recovery",
                UnsafeClaimReasonCategory::AgentMailReadiness,
            ),
            (
                "archive_index_parity_drift",
                UnsafeClaimReasonCategory::AgentMailReadiness,
            ),
            (
                "archive_corruption",
                UnsafeClaimReasonCategory::AgentMailReadiness,
            ),
            (
                "green_transport_does_not_imply_authoritative_reads",
                UnsafeClaimReasonCategory::AgentMailReadiness,
            ),
            (
                "active_claim",
                UnsafeClaimReasonCategory::ReservationConflict,
            ),
            (
                "fallback_row_already_owned",
                UnsafeClaimReasonCategory::ReservationConflict,
            ),
            (
                "reservation_evidence_stale",
                UnsafeClaimReasonCategory::ReservationConflict,
            ),
            (
                "candidate_assigned_to:OtherAgent",
                UnsafeClaimReasonCategory::ReservationConflict,
            ),
            (
                "active_owner_or_compile_health_blocker_present",
                UnsafeClaimReasonCategory::ReservationConflict,
            ),
            (
                "reservation_collision:src/core/*.rs",
                UnsafeClaimReasonCategory::ReservationConflict,
            ),
            (
                "related_bead_collision:docs/schemas/swarm",
                UnsafeClaimReasonCategory::SourceOverlap,
            ),
            (
                "file_collision:high:src/core/*.rs",
                UnsafeClaimReasonCategory::SourceOverlap,
            ),
            (
                "file_collision_owner:GrayJaguar:src/core/*.rs",
                UnsafeClaimReasonCategory::SourceOverlap,
            ),
            (
                "file_collision_related_bead:bd-owned",
                UnsafeClaimReasonCategory::SourceOverlap,
            ),
            (
                "unproved_same_file_source_debt",
                UnsafeClaimReasonCategory::SourceOverlap,
            ),
            (
                "high_risk_dirty_surface:src/core/*.rs",
                UnsafeClaimReasonCategory::SourceOverlap,
            ),
            (
                "rch_verify_capacity_or_timeout",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "source_authority_rch_degraded_read_only",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "dirty_compile_health_blocks_rch",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "source_authority_installed_binary_stale_fallback",
                UnsafeClaimReasonCategory::InstalledBinaryFreshness,
            ),
            (
                "source_authority_memory_drift_degraded_read_only",
                UnsafeClaimReasonCategory::MemorySourceDrift,
            ),
            (
                "active_project_exclusion",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "all_workers_preflight_failed",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "build_admission_blocked",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "capacity_or_timeout",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "command_not_offloaded",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "insufficient_slots",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "no_admissible_workers",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "no_worker_selected",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "no_workers_passed_health",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "no_workers_with_rust_installed",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "remote_marker_missing",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "recent_verifier_status:failed",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "selector_admission_failed",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "topology_blocked",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "worker_health_threshold",
                UnsafeClaimReasonCategory::RchProofAdmission,
            ),
            (
                "install_freshness:stale",
                UnsafeClaimReasonCategory::InstalledBinaryFreshness,
            ),
            (
                "stale_binary_suspected",
                UnsafeClaimReasonCategory::InstalledBinaryFreshness,
            ),
            (
                "stale_claim_gate_binary",
                UnsafeClaimReasonCategory::InstalledBinaryFreshness,
            ),
            (
                "unsupported_claim_gate_binary",
                UnsafeClaimReasonCategory::InstalledBinaryFreshness,
            ),
            (
                "missing_required_surface",
                UnsafeClaimReasonCategory::InstalledBinaryFreshness,
            ),
            (
                "bv_advisory_contradiction:bd-1",
                UnsafeClaimReasonCategory::BvStaleness,
            ),
            ("bv_unavailable", UnsafeClaimReasonCategory::BvStaleness),
            (
                "bv_recommendation_stale",
                UnsafeClaimReasonCategory::BvStaleness,
            ),
            (
                "bv_recommends_id_absent_from_actionable_queue:bd-1",
                UnsafeClaimReasonCategory::BvStaleness,
            ),
            (
                "graph_triage_unavailable",
                UnsafeClaimReasonCategory::BvStaleness,
            ),
            (
                "actionable_queue_candidate_absent:bd-1",
                UnsafeClaimReasonCategory::RecommendationMismatch,
            ),
            (
                "candidate_already_appears_in_multiple_sources",
                UnsafeClaimReasonCategory::RecommendationMismatch,
            ),
            (
                "candidate_status:in_progress",
                UnsafeClaimReasonCategory::RecommendationMismatch,
            ),
            (
                "candidate_issue_type:epic",
                UnsafeClaimReasonCategory::RecommendationMismatch,
            ),
            (
                "rollup_candidate_not_claimable",
                UnsafeClaimReasonCategory::RecommendationMismatch,
            ),
            (
                "claim_concrete_child_bead_instead",
                UnsafeClaimReasonCategory::RecommendationMismatch,
            ),
            (
                "blocked_by:bd-parent",
                UnsafeClaimReasonCategory::RecommendationMismatch,
            ),
            (
                "packet_recommendation_candidate_mismatch:bd-1:bd-2",
                UnsafeClaimReasonCategory::RecommendationMismatch,
            ),
            (
                "candidate_is_rollup_not_leaf",
                UnsafeClaimReasonCategory::RecommendationMismatch,
            ),
            (
                "rollup_has_no_claimable_child",
                UnsafeClaimReasonCategory::RecommendationMismatch,
            ),
            (
                "memory_probe_unavailable",
                UnsafeClaimReasonCategory::MemorySourceDrift,
            ),
            (
                "memory_drift_lock_contention",
                UnsafeClaimReasonCategory::ResourceAdmission,
            ),
            (
                "disk_pressure_critical",
                UnsafeClaimReasonCategory::ResourceAdmission,
            ),
            (
                "cache_pressure",
                UnsafeClaimReasonCategory::ResourceAdmission,
            ),
            (
                "local_cargo_bypass_detected",
                UnsafeClaimReasonCategory::ActionSuppression,
            ),
            (
                "release_operator_required:crates_io_publish",
                UnsafeClaimReasonCategory::ActionSuppression,
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

    #[test]
    fn known_gate_vocabulary_drives_expected_action_families() -> TestResult {
        for (reason, category, action_kind) in [
            (
                "local_cargo_bypass_detected",
                UnsafeClaimReasonCategory::ActionSuppression,
                UnsafeClaimActionKind::Stop,
            ),
            (
                "stale_binary_suspected",
                UnsafeClaimReasonCategory::InstalledBinaryFreshness,
                UnsafeClaimActionKind::Inspect,
            ),
            (
                "missing_required_surface",
                UnsafeClaimReasonCategory::InstalledBinaryFreshness,
                UnsafeClaimActionKind::Inspect,
            ),
            (
                "build_admission_blocked",
                UnsafeClaimReasonCategory::RchProofAdmission,
                UnsafeClaimActionKind::WaitOrCoordinate,
            ),
            (
                "active_project_exclusion",
                UnsafeClaimReasonCategory::RchProofAdmission,
                UnsafeClaimActionKind::WaitOrCoordinate,
            ),
            (
                "capacity_or_timeout",
                UnsafeClaimReasonCategory::RchProofAdmission,
                UnsafeClaimActionKind::WaitOrCoordinate,
            ),
            (
                "no_workers_passed_health",
                UnsafeClaimReasonCategory::RchProofAdmission,
                UnsafeClaimActionKind::WaitOrCoordinate,
            ),
            (
                "rch_remote_required_fallback_prevented",
                UnsafeClaimReasonCategory::RchProofAdmission,
                UnsafeClaimActionKind::WaitOrCoordinate,
            ),
            (
                "bv_recommendation_stale",
                UnsafeClaimReasonCategory::BvStaleness,
                UnsafeClaimActionKind::AlternateCandidate,
            ),
            (
                "disk_pressure_critical",
                UnsafeClaimReasonCategory::ResourceAdmission,
                UnsafeClaimActionKind::WaitOrCoordinate,
            ),
            (
                "cache_pressure",
                UnsafeClaimReasonCategory::ResourceAdmission,
                UnsafeClaimActionKind::WaitOrCoordinate,
            ),
            // bd-1n3x1.16.7 fresh-eyes: live Agent Mail corruption coordination
            // strings drive the read-only retry_with_snapshot family, not unknown.
            (
                "archive_corruption",
                UnsafeClaimReasonCategory::AgentMailReadiness,
                UnsafeClaimActionKind::RetryWithSnapshot,
            ),
            (
                "green_transport_does_not_imply_authoritative_reads",
                UnsafeClaimReasonCategory::AgentMailReadiness,
                UnsafeClaimActionKind::RetryWithSnapshot,
            ),
            // bd-1n3x1.16.7 fresh-eyes: same-file proof debt is a source overlap,
            // so it must drive decompose_candidate, not unknown/inspect.
            (
                "unproved_same_file_source_debt",
                UnsafeClaimReasonCategory::SourceOverlap,
                UnsafeClaimActionKind::DecomposeCandidate,
            ),
        ] {
            let classification = classify_unsafe_claim_evidence(&strings(&[reason]), &[]);
            ensure(
                classification
                    .reason_groups
                    .iter()
                    .any(|group| group.category == category),
                format!("{reason}: expected group {category:?}"),
            )?;
            ensure(
                has_action(&classification, action_kind, category.as_str()),
                format!(
                    "{reason}: expected action {action_kind:?} for {}",
                    category.as_str()
                ),
            )?;
        }
        Ok(())
    }

    /// bd-1n3x1.16.3: a broad feature with dirty source overlaps suggests
    /// per-surface leaves (repository/domain/CLI/proof style) without
    /// mutating anything.
    #[test]
    fn broad_feature_with_source_overlaps_suggests_surface_leaves() -> TestResult {
        let classification = classify_unsafe_claim_evidence(
            &strings(&[
                "source_overlap:src/db/mod.rs",
                "dirty_checkout:src/cli/mod.rs",
                "reservation_collision:src/core/*.rs",
            ]),
            &[],
        );
        let facts = UnsafeClaimCandidateFacts {
            candidate_id: "bd-test1".to_owned(),
            title: "situation storage: persist classifications".to_owned(),
            description: "broad feature description".repeat(40),
            issue_type: "feature".to_owned(),
            priority: Some(2),
            labels: vec![],
            path_families: vec![
                "src/db/".to_owned(),
                "src/core/".to_owned(),
                "src/cli/".to_owned(),
                "tests/".to_owned(),
            ],
        };
        let plan = suggest_unsafe_claim_decomposition(&facts, &classification);
        ensure(
            plan.decompose,
            "broad + overlap evidence must recommend a split",
        )?;
        ensure(
            plan.suggested_beads.len() == 4,
            format!(
                "one leaf per path family, got {}",
                plan.suggested_beads.len()
            ),
        )?;
        let lanes: Vec<&str> = plan
            .suggested_beads
            .iter()
            .flat_map(|bead| bead.labels.iter())
            .filter_map(|label| label.strip_prefix("lane:"))
            .collect();
        ensure(
            lanes == vec!["storage", "domain", "cli", "tests"],
            format!("deterministic lane order drifted: {lanes:?}"),
        )?;
        ensure(
            plan.suggested_beads
                .iter()
                .all(|bead| bead.parent_id == "bd-test1" && bead.issue_type == "task"),
            "leaves must parent to the candidate as tasks",
        )?;
        ensure(
            plan.comment_template.contains("source_overlap")
                && !plan.comment_template.contains("src/db/mod.rs"),
            "the comment cites categories, never raw path evidence",
        )?;
        ensure(
            plan.overlap_search_terms.contains(&"bd-test1".to_owned()),
            "overlap terms must include the candidate id",
        )
    }

    /// bd-1n3x1.16.3 negative fixture: a narrow leaf unsafe only because
    /// of coordination evidence must NOT be split.
    #[test]
    fn narrow_leaf_or_unroutable_blockers_do_not_decompose() -> TestResult {
        // Narrow leaf with an overlap blocker: the surface IS the work.
        let overlap =
            classify_unsafe_claim_evidence(&strings(&["reservation_collision:src/core/x.rs"]), &[]);
        let narrow = UnsafeClaimCandidateFacts {
            candidate_id: "bd-leaf".to_owned(),
            title: "fix one assertion".to_owned(),
            description: "small".to_owned(),
            issue_type: "bug".to_owned(),
            priority: Some(2),
            labels: vec![],
            path_families: vec!["src/core/".to_owned()],
        };
        let plan = suggest_unsafe_claim_decomposition(&narrow, &overlap);
        ensure(!plan.decompose, "narrow leaves must not be split")?;
        ensure(
            plan.suggested_beads.is_empty(),
            "no drafts when not decomposing",
        )?;

        // Broad bead whose only blocker is missing Agent Mail evidence:
        // splitting cannot route around it.
        let mail_only = classify_unsafe_claim_evidence(&strings(&["agent_mail_unavailable"]), &[]);
        let broad = UnsafeClaimCandidateFacts {
            candidate_id: "bd-broad".to_owned(),
            title: "[idea-wizard] broad feature".to_owned(),
            description: "x".repeat(700),
            issue_type: "feature".to_owned(),
            priority: None,
            labels: vec![],
            path_families: vec!["src/db/".to_owned(), "src/cli/".to_owned()],
        };
        let plan = suggest_unsafe_claim_decomposition(&broad, &mail_only);
        ensure(
            !plan.decompose,
            "blockers splitting cannot fix must not trigger a split",
        )?;
        ensure(
            plan.reason.contains("would not route around"),
            "the refusal explains itself",
        )
    }

    /// bd-1n3x1.16.3: a broad bead with NO path families still gets the
    /// contract → implementation → proof ladder.
    #[test]
    fn broad_bead_without_path_families_gets_standard_ladder() -> TestResult {
        let classification =
            classify_unsafe_claim_evidence(&strings(&["dirty_checkout_pending_peer_commits"]), &[]);
        let facts = UnsafeClaimCandidateFacts {
            candidate_id: "bd-nofam".to_owned(),
            title: "[CLUSTER] sweeping refactor".to_owned(),
            description: String::new(),
            issue_type: "epic".to_owned(),
            priority: Some(1),
            labels: vec![],
            path_families: vec![],
        };
        let plan = suggest_unsafe_claim_decomposition(&facts, &classification);
        ensure(plan.decompose, "broad cluster must decompose")?;
        let lanes: Vec<&str> = plan
            .suggested_beads
            .iter()
            .flat_map(|bead| bead.labels.iter())
            .filter_map(|label| label.strip_prefix("lane:"))
            .collect();
        ensure(
            lanes == vec!["contract", "implementation", "proof"],
            format!("standard ladder drifted: {lanes:?}"),
        )?;
        ensure(
            plan.suggested_beads.iter().all(|bead| bead.priority == 1),
            "leaves inherit the parent priority",
        )
    }

    fn alternate_candidate(
        id: &str,
        title: &str,
        priority: i64,
        score: i64,
        labels: &[&str],
        paths: &[&str],
    ) -> UnsafeClaimAlternateCandidateFacts {
        UnsafeClaimAlternateCandidateFacts {
            candidate_id: id.to_owned(),
            title: title.to_owned(),
            issue_type: "task".to_owned(),
            status: "open".to_owned(),
            assignee: None,
            priority: Some(priority),
            score,
            labels: strings(labels),
            path_families: strings(paths),
            gate_verdict: None,
            gate_safe_to_claim: None,
            gate_claim_command_action_present: false,
            evidence_freshness: "fresh".to_owned(),
            reason_group_refs: Vec::new(),
            candidate_specific_deltas: Vec::new(),
        }
    }

    /// bd-1n3x1.16.4: when source authority is degraded, low-blast
    /// tracker/docs/design/fixture/shell work ranks ahead of Rust source
    /// work, and every unscanned alternate still needs its own claim gate.
    #[test]
    fn alternate_candidates_prefer_low_blast_radius_when_authority_degraded() -> TestResult {
        let input = UnsafeClaimAlternatePlannerInput {
            requested_candidate_id: Some("bd-source".to_owned()),
            source_authority_degraded: true,
            tracker_health: "external_changes_pending_import".to_owned(),
            agent_mail_status: "degraded_read_only".to_owned(),
            source_freshness: "stale".to_owned(),
            unsafe_path_families: strings(&["src/core"]),
            candidates: vec![
                alternate_candidate(
                    "bd-source",
                    "unsafe source implementation leaf",
                    1,
                    90,
                    &["rust"],
                    &["src/core"],
                ),
                alternate_candidate(
                    "bd-docs",
                    "docs-only unsafe plan notes",
                    1,
                    80,
                    &["docs"],
                    &["docs/swarm"],
                ),
                alternate_candidate(
                    "bd-tracker",
                    "tracker-only closeout hygiene",
                    2,
                    10,
                    &["tracker-only"],
                    &[],
                ),
                alternate_candidate(
                    "bd-fixture",
                    "fixture-only unsafe plan coverage",
                    1,
                    100,
                    &["fixture-only"],
                    &["tests/fixtures/swarm"],
                ),
            ],
        };

        let plan = recommend_unsafe_claim_alternates(&input);
        ensure(
            plan.recommended_action == "gate_plausible_alternate",
            format!("unexpected recommended action {}", plan.recommended_action),
        )?;
        let ids: Vec<&str> = plan
            .candidates
            .iter()
            .map(|candidate| candidate.candidate_id.as_str())
            .collect();
        ensure(
            ids == vec!["bd-tracker", "bd-docs", "bd-fixture", "bd-source"],
            format!("degraded-authority alternate order drifted: {ids:?}"),
        )?;
        ensure(
            plan.candidates
                .iter()
                .filter(|candidate| {
                    candidate.candidate_state
                        == UnsafeClaimAlternateCandidateState::PlausibleButRequiresGate
                })
                .all(|candidate| {
                    candidate.needs_fresh_claim_gate && !candidate.may_emit_claim_command
                }),
            "plausible alternates must require a fresh gate and emit no claim command",
        )?;
        let top = plan.candidates.first().ok_or("missing top alternate")?;
        ensure(
            top.next_command_actions.iter().any(|action| {
                action.argv == strings(&["br", "show", "bd-tracker", "--json"])
                    && !action.mutates_state
            }),
            "top alternate must include a read-only br show action",
        )?;
        ensure(
            top.next_command_actions.iter().any(|action| {
                action.argv
                    == strings(&[
                        "ee",
                        "swarm",
                        "work-packet",
                        "--workspace",
                        ".",
                        "--include-rch",
                        "--claim-gate",
                        "--candidate",
                        "bd-tracker",
                        "--json",
                    ])
                    && !action.mutates_state
            }),
            "top alternate must include a read-only claim-gate retry action",
        )
    }

    /// bd-1n3x1.16.4: if every alternate is owned, blocked, epic, or
    /// already scanned unsafe, the planner stops instead of inventing work.
    #[test]
    fn alternate_candidate_plan_stops_when_no_plausible_alternate_exists() -> TestResult {
        let mut owned = alternate_candidate("bd-owned", "owned leaf", 1, 50, &[], &["docs"]);
        owned.assignee = Some("OtherAgent".to_owned());
        let mut epic = alternate_candidate("bd-epic", "rollup", 1, 90, &[], &[]);
        epic.issue_type = "epic".to_owned();
        let mut scanned = alternate_candidate("bd-scanned", "scanned unsafe", 1, 70, &[], &[]);
        scanned.gate_verdict = Some("unsafe_due_to_conflict".to_owned());
        scanned.gate_safe_to_claim = Some(false);
        scanned.reason_group_refs = vec!["rg-tracker-authority".to_owned()];

        let plan = recommend_unsafe_claim_alternates(&UnsafeClaimAlternatePlannerInput {
            requested_candidate_id: Some("bd-requested".to_owned()),
            source_authority_degraded: true,
            tracker_health: "stale".to_owned(),
            agent_mail_status: "unavailable".to_owned(),
            source_freshness: "unknown".to_owned(),
            unsafe_path_families: strings(&[]),
            candidates: vec![owned, epic, scanned],
        });

        ensure(
            plan.recommended_action == "stop_or_coordinate",
            format!(
                "expected stop_or_coordinate, got {}",
                plan.recommended_action
            ),
        )?;
        ensure(
            !plan.candidates.iter().any(|candidate| {
                candidate.candidate_state
                    == UnsafeClaimAlternateCandidateState::PlausibleButRequiresGate
                    || candidate.candidate_state
                        == UnsafeClaimAlternateCandidateState::FreshSafeToClaim
            }),
            "no plausible or fresh-safe candidates should be invented",
        )?;
        ensure(
            plan.candidates
                .iter()
                .all(|candidate| !candidate.may_emit_claim_command),
            "unsafe-plan alternates never emit claim commands",
        )?;
        let scanned = plan
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_id == "bd-scanned")
            .ok_or("missing scanned alternate")?;
        ensure(
            scanned.candidate_state == UnsafeClaimAlternateCandidateState::ScannedAndUnsafe
                && !scanned.needs_fresh_claim_gate,
            "already-scanned unsafe alternates must not request another fresh gate",
        )?;
        ensure(
            scanned
                .next_command_actions
                .iter()
                .all(|action| !action.command_id.starts_with("claim_gate_retry:")),
            "already-scanned unsafe alternates must remain inspect-only",
        )?;
        ensure(
            plan.next_command_actions == vec![bead_show_action("bd-scanned")],
            "stop_or_coordinate plans should expose only read-only inspection for the top scanned-unsafe alternate",
        )
    }

    /// bd-1n3x1.16.4: deterministic ordering within a class is priority,
    /// then score, evidence freshness, and candidate id.
    #[test]
    fn alternate_candidate_tie_breaks_are_deterministic() -> TestResult {
        let mut stale = alternate_candidate("bd-stale", "docs leaf", 1, 90, &["docs"], &["docs"]);
        stale.evidence_freshness = "stale".to_owned();
        let mut fresh_b = alternate_candidate("bd-b", "docs leaf", 1, 90, &["docs"], &["docs"]);
        fresh_b.evidence_freshness = "fresh".to_owned();
        let mut fresh_a = alternate_candidate("bd-a", "docs leaf", 1, 90, &["docs"], &["docs"]);
        fresh_a.evidence_freshness = "fresh".to_owned();
        let lower_priority =
            alternate_candidate("bd-priority2", "docs leaf", 2, 500, &["docs"], &["docs"]);
        let lower_score =
            alternate_candidate("bd-low-score", "docs leaf", 1, 50, &["docs"], &["docs"]);

        let plan = recommend_unsafe_claim_alternates(&UnsafeClaimAlternatePlannerInput {
            requested_candidate_id: None,
            source_authority_degraded: false,
            tracker_health: "fresh".to_owned(),
            agent_mail_status: "fresh".to_owned(),
            source_freshness: "fresh".to_owned(),
            unsafe_path_families: Vec::new(),
            candidates: vec![stale, fresh_b, lower_priority, lower_score, fresh_a],
        });
        let ids: Vec<&str> = plan
            .candidates
            .iter()
            .map(|candidate| candidate.candidate_id.as_str())
            .collect();
        ensure(
            ids == vec!["bd-a", "bd-b", "bd-stale", "bd-low-score", "bd-priority2"],
            format!("tie-break order drifted: {ids:?}"),
        )?;
        ensure(
            plan.candidates
                .iter()
                .enumerate()
                .all(|(index, candidate)| candidate.rank == index + 1),
            "rank fields must match emitted order",
        )
    }

    // ─── bd-1n3x1.16: ee.swarm.unsafe_claim_plan.v1 envelope assembler ───

    fn sample_unsafe_plan_input() -> UnsafeClaimPlanInput {
        UnsafeClaimPlanInput {
            generated_at: "2026-06-14T03:00:00Z".to_owned(),
            workspace: "repo:25e38e130474e7f0292de2a3".to_owned(),
            gate_id: "swarm_work_packet_claim_gate_0123456789abcdef01234567".to_owned(),
            packet_id: "swarm_work_packet_0123456789abcdef01234567".to_owned(),
            requested_candidate_id: Some("bd-1n3x1.16".to_owned()),
            selected_candidate_id: Some("bd-1n3x1.16".to_owned()),
            verdict: "unsafe_due_to_conflict".to_owned(),
            recommended_action: "coordinate_before_claim".to_owned(),
            recommended_safe_to_claim: Some(false),
            unsafe_reasons: strings(&[
                "beads_tracker_not_authoritative:external_changes_pending_import",
                "archive_corruption",
                "file_collision:high:src/core/*.rs",
                "unproved_same_file_source_debt",
                "dirty_checkout_path_count:19",
                "future_gate_reason:opaque_value",
            ]),
            stale_reasons: strings(&["tracker_authority_external_changes_pending_import"]),
            degraded_codes: strings(&[
                "agent_mail_unavailable",
                "bv_command_timeout",
                "rch_remote_required_fallback_prevented",
            ]),
            source_refs: strings(&["br://bd-1n3x1.16"]),
            next_command_actions: Vec::new(),
        }
    }

    #[test]
    fn unsafe_claim_plan_has_complete_schema_shape() -> TestResult {
        let plan = build_unsafe_claim_plan(&sample_unsafe_plan_input());
        ensure(plan.schema == "ee.swarm.unsafe_claim_plan.v1", "schema id")?;
        let suffix = plan
            .plan_id
            .strip_prefix("unsafe_claim_plan_")
            .ok_or_else(|| format!("planId prefix drifted: {}", plan.plan_id))?;
        ensure(
            suffix.len() == 24
                && suffix.chars().all(|character| {
                    character.is_ascii_hexdigit() && !character.is_ascii_uppercase()
                }),
            format!("planId must be 24 lowercase hex: {}", plan.plan_id),
        )?;
        ensure(
            plan.provenance_hash.starts_with("blake3:")
                && plan.provenance_hash.len() == "blake3:".len() + 64,
            format!("provenanceHash shape: {}", plan.provenance_hash),
        )?;
        ensure(
            plan.redaction_status == UNSAFE_CLAIM_PLAN_REDACTION_STATUS,
            "redactionStatus const",
        )?;
        ensure(
            plan.ordering
                .reason_groups
                .contains("reasonCategory enum order")
                && plan
                    .ordering
                    .evidence_sources
                    .contains("sourceId ascending"),
            "ordering consts",
        )?;
        ensure(
            plan.non_mutation_policy.advisory_only
                && !plan.non_mutation_policy.claims_beads
                && !plan.non_mutation_policy.reserves_files
                && !plan.non_mutation_policy.sends_agent_mail
                && !plan.non_mutation_policy.runs_cargo
                && !plan.non_mutation_policy.stages_git
                && !plan.non_mutation_policy.deletes_files,
            "nonMutationPolicy must be all-advisory",
        )?;
        ensure(plan.candidate_plans.len() == 1, "candidatePlans minItems 1")?;
        ensure(!plan.reason_groups.is_empty(), "reasonGroups present")?;
        ensure(
            plan.reason_groups.iter().all(|group| {
                group.group_id.starts_with("rg-")
                    && !group.evidence_source_ids.is_empty()
                    && !group.source_fields.is_empty()
                    && !group.candidate_coverage.is_empty()
            }),
            "reasonGroup enrichment fields populated",
        )?;
        ensure(
            plan.source_gate.verdict == "unsafe_due_to_conflict"
                && !plan.source_gate.safe_to_claim
                && plan.source_gate.claim_command_action.is_none()
                && plan.source_gate.schema == "ee.swarm.work_packet.claim_gate.v1",
            "sourceGate echo",
        )?;
        let candidate = &plan.candidate_plans[0];
        ensure(
            !candidate.may_emit_claim_command
                && candidate.needs_fresh_claim_gate
                && !candidate.safe_to_claim,
            "candidatePlan invariants",
        )?;
        // Every action/group cross-reference must resolve to an emitted id.
        let group_ids: std::collections::BTreeSet<&str> = plan
            .reason_groups
            .iter()
            .map(|group| group.group_id.as_str())
            .collect();
        let source_ids: std::collections::BTreeSet<&str> = plan
            .evidence_sources
            .iter()
            .map(|source| source.source_id.as_str())
            .collect();
        for action in &plan.planner_actions {
            ensure(
                action.action_id.starts_with("act-")
                    && !action.mutates_state
                    && action.advisory_only,
                "plannerAction invariants",
            )?;
            for reference in &action.reason_group_refs {
                ensure(
                    group_ids.contains(reference.as_str()),
                    format!("action reasonGroupRef {reference} must exist"),
                )?;
            }
            for reference in &action.evidence_source_ids {
                ensure(
                    source_ids.contains(reference.as_str()),
                    format!("action evidenceSourceId {reference} must exist"),
                )?;
            }
        }
        // evidenceSources sorted by sourceId.
        let mut sorted_ids = source_ids.iter().copied().collect::<Vec<_>>();
        sorted_ids.sort_unstable();
        ensure(
            plan.evidence_sources
                .iter()
                .map(|source| source.source_id.as_str())
                .eq(sorted_ids.into_iter()),
            "evidenceSources must be sorted by sourceId",
        )
    }

    #[test]
    fn unsafe_claim_plan_id_excludes_generated_at() -> TestResult {
        let mut early = sample_unsafe_plan_input();
        let mut late = sample_unsafe_plan_input();
        early.generated_at = "2020-01-01T00:00:00Z".to_owned();
        late.generated_at = "2031-12-31T23:59:59Z".to_owned();
        let plan_early = build_unsafe_claim_plan(&early);
        let plan_late = build_unsafe_claim_plan(&late);
        ensure(
            plan_early.plan_id == plan_late.plan_id,
            "planId must be stable across generatedAt",
        )?;
        ensure(
            plan_early.provenance_hash == plan_late.provenance_hash,
            "provenanceHash must be stable across generatedAt",
        )?;
        ensure(
            plan_early.generated_at != plan_late.generated_at,
            "generatedAt itself must still reflect the input",
        )
    }

    #[test]
    fn unsafe_claim_plan_preserves_unknown_reasons() -> TestResult {
        let plan = build_unsafe_claim_plan(&sample_unsafe_plan_input());
        let unknown = plan
            .reason_groups
            .iter()
            .find(|group| group.category == "unknown")
            .ok_or("unknown reason group missing")?;
        ensure(unknown.preserves_unknown, "preservesUnknown must be true")?;
        ensure(
            unknown.candidate_coverage == vec!["unknown"],
            "unknown candidateCoverage",
        )?;
        ensure(
            unknown
                .reason_codes
                .iter()
                .any(|code| code.contains("future_gate_reason")),
            "unrecognized reason preserved verbatim",
        )
    }

    #[test]
    fn unsafe_claim_plan_actions_map_known_families() -> TestResult {
        let plan = build_unsafe_claim_plan(&sample_unsafe_plan_input());
        let has_family = |kind: &str, group: &str| {
            plan.planner_actions.iter().any(|action| {
                action.kind == kind
                    && action
                        .reason_group_refs
                        .iter()
                        .any(|reference| reference == group)
            })
        };
        ensure(
            has_family("retry_with_snapshot", "rg-agent-mail-readiness"),
            "agent_mail readiness must drive retry_with_snapshot",
        )?;
        ensure(
            has_family("decompose_candidate", "rg-source-overlap"),
            "source overlap must drive decompose_candidate",
        )?;
        let retry = plan
            .planner_actions
            .iter()
            .find(|action| action.kind == "retry_with_snapshot")
            .ok_or("retry action missing")?;
        let command = retry
            .command_action
            .as_ref()
            .ok_or("retry action must carry a read-only command")?;
        ensure(
            !command.shell_required
                && command.copy_safety == "safe_structured_argv"
                && !command.mutates_state,
            "retry command must be a safe read-only argv",
        )
    }

    #[test]
    fn unsafe_claim_plan_is_redaction_safe() -> TestResult {
        let plan = build_unsafe_claim_plan(&sample_unsafe_plan_input());
        let json = serde_json::to_string(&plan).map_err(|error| error.to_string())?;
        for forbidden in [
            "/Users/",
            "/home/",
            "BEGIN PRIVATE KEY",
            "ghp_",
            "Bearer ",
            "DATABASE_URL=",
            "raw_inbox",
            "stdout:",
            "stderr:",
        ] {
            ensure(
                !json.contains(forbidden),
                format!("plan leaked forbidden token {forbidden}"),
            )?;
        }
        Ok(())
    }
}
