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
        "inbox_evidence_not_authoritative" | "archive_index_parity_drift" => {
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
        // bd-1xpq9: lock contention blocked collection BEFORE evidence
        // inspection — a contended local resource, not drift evidence.
        "memory_drift_lock_contention" => UnsafeClaimReasonCategory::ResourceAdmission,
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
        let needs_fresh_claim_gate = state
            == UnsafeClaimAlternateCandidateState::PlausibleButRequiresGate
            || state == UnsafeClaimAlternateCandidateState::ScannedAndUnsafe;

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
        if state != UnsafeClaimAlternateCandidateState::BlockedOrOwned
            && state != UnsafeClaimAlternateCandidateState::NotFound
        {
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
    fn lock_contention_stays_resource_admission_without_hiding_memory_drift() -> TestResult {
        let classification = classify_unsafe_claim_evidence(
            &strings(&[
                "memory_drift_lock_contention",
                "memory_drift_source_unverifiable",
            ]),
            &strings(&["memory_drift_lock_contention"]),
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
            "resource-admission lock contention must recommend wait_or_coordinate",
        )?;
        ensure(
            classification
                .planner_actions
                .iter()
                .all(|action| action.advisory_only && !action.mutates_state),
            "lock-contention planner actions must stay advisory-only",
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
}
