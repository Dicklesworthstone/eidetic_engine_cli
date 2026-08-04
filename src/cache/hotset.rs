//! Redaction-safe hotset manifest for search/context cache prewarm.
//!
//! Persists frequent query shapes, memory IDs, index generations, profile
//! tier, and hit counts without storing raw query text or memory content.
//! The manifest is the durable, auditable record swarm operators ship into
//! support bundles or hand off to a future `ee cache prewarm` surface so a
//! read-heavy burst can warm caches against the same shapes the previous
//! workload exercised.
//!
//! Inputs are the existing `SearchHotsetEntry` and `PackHotsetEntry` records
//! produced by `src/search/mod.rs` and `src/pack/mod.rs`. Both entry types
//! already store hashes, kind tags, generation, estimated bytes, hit counts,
//! and a `redaction_status` marker — no plaintext content. This module wraps
//! them in a stable `ee.cache.hotset.v1` artifact, classifies stale entries
//! against the current `(workspace_generation, index_generation)` gate, and
//! emits a `cache_hotset_stale` degradation when stale entries were rejected
//! so agents can choose to recapture instead of silently warming with stale
//! candidates.
//!
//! The module is process-local and side-effect free: it does NOT read or
//! write any cache, file, or database. Caller decides what to do with the
//! manifest (write to disk, ship in a support bundle, hand to a prewarm
//! command). All ordering is deterministic so identical inputs produce
//! byte-identical JSON after the caller strips volatile fields such as
//! `capturedAt`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::cache::{CacheBudget, MemoryPressure};
use crate::pack::{
    PackCacheGovernor, PackHotset, PackHotsetEntry, PackHotsetEntryKind, PackSection,
    prewarm_pack_hotset,
};
use crate::search::{
    SearchCacheGovernor, SearchHotset, SearchHotsetEntry, SearchHotsetEntryKind,
    prewarm_search_hotset,
};

/// JSON Schema id pinned by every emitted manifest.
pub const SCHEMA: &str = "ee.cache.hotset.v1";

/// Degraded code emitted when the manifest rejected stale entries (their
/// `generation` is older than the gate's `workspace_generation` or
/// `index_generation`). Severity is `medium`: warming caches with stale
/// shapes would silently degrade pack quality if the rejected entries were
/// admitted, so the manifest filters them and surfaces the rejection.
pub const STALE_HOTSET_CODE: &str = "cache_hotset_stale";

/// The single redaction posture this manifest claims. Mirrors the
/// `content_not_stored` marker each entry carries inside the search/pack
/// hotset structs. If any entry carries a different marker the manifest
/// refuses to admit it (see [`HotsetManifest::is_redaction_safe`]).
pub const REDACTION_STATUS: &str = "content_not_stored";

/// JSON Schema id for the advisory dry-run plan that predicts context
/// hotsets from swarm coordination signals.
pub const PREWARM_PLAN_SCHEMA: &str = "ee.cache.hotset_prewarm_plan.v1";

/// JSON Schema id for the explicit `ee cache prewarm` report.
pub const CACHE_PREWARM_SCHEMA: &str = "ee.cache.prewarm.v1";

/// Degraded code emitted when the prewarm planner receives no usable signal.
pub const PREWARM_NO_SIGNAL_CODE: &str = "hotset_prewarm_no_signals";

/// Degraded code emitted when tier-aware prewarm rejects stale memory tier
/// metadata instead of using it to bias cache residency.
pub const MEMORY_TIER_METADATA_STALE_CODE: &str = "memory_tier_metadata_stale";

/// Redaction posture for prewarm plans. Query text, mail bodies, bead titles,
/// and other raw coordination text are used only in-process to derive BLAKE3
/// query-shape keys; the plan itself exposes hashes and source classes.
pub const PREWARM_REDACTION_STATUS: &str = "query_hashes_only";

/// Schema id for the pure memory tier policy report. The first slice is a
/// side-effect-free model only; retrieval and storage admission stay unchanged.
pub const MEMORY_TIER_POLICY_SCHEMA: &str = "ee.memory_tier.policy.v1";

/// Version string recorded on every tier assignment for audit metadata.
pub const MEMORY_TIER_POLICY_VERSION: &str = "memory-tier-policy-v1";

/// Generation gate the manifest evaluates entries against. Entries whose
/// `generation` is strictly less than the active workspace generation or the
/// index generation that produced them are classified as stale-rejected.
///
/// Note: the search and pack entry types share a single `generation` field
/// today; this struct keeps both fields so a future split (workspace-rev
/// versus index-rev) does not require renaming the schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationGate {
    pub workspace_generation: u64,
    pub index_generation: u64,
}

impl GenerationGate {
    /// Construct a gate from explicit generations.
    #[must_use]
    pub const fn new(workspace_generation: u64, index_generation: u64) -> Self {
        Self {
            workspace_generation,
            index_generation,
        }
    }

    /// The minimum generation an entry must carry to be admitted. Today both
    /// hotset entry families use a single `generation`, so the admission
    /// threshold is the higher of the two — admitting an entry from a stale
    /// index against a fresh workspace would silently warm cold-mass.
    #[must_use]
    pub const fn admission_threshold(self) -> u64 {
        if self.workspace_generation > self.index_generation {
            self.workspace_generation
        } else {
            self.index_generation
        }
    }
}

/// Memory budget the manifest reports for operator visibility. Numeric values
/// are advisory: the manifest itself does not evict, but the budget travels
/// with the artifact so a follow-up prewarm command can refuse admission when
/// `current_*` already meets or exceeds `max_*`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HotsetBudget {
    pub max_entries: usize,
    pub max_bytes: usize,
    pub current_entries: usize,
    pub current_bytes: usize,
}

impl HotsetBudget {
    #[must_use]
    pub const fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            current_entries: 0,
            current_bytes: 0,
        }
    }

    #[must_use]
    pub const fn with_current(mut self, current_entries: usize, current_bytes: usize) -> Self {
        self.current_entries = current_entries;
        self.current_bytes = current_bytes;
        self
    }

    fn to_json(self) -> Value {
        json!({
            "maxEntries": self.max_entries,
            "maxBytes": self.max_bytes,
            "currentEntries": self.current_entries,
            "currentBytes": self.current_bytes,
        })
    }
}

/// Advisory storage tier for memory recall hot paths.
///
/// This is not an eligibility decision. A cold item can still be required
/// retrieval evidence when it is an explicit query match, mandatory provenance,
/// or safety/failure evidence.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MemoryStorageTier {
    Hot,
    Warm,
    Cold,
}

impl MemoryStorageTier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Warm => "warm",
            Self::Cold => "cold",
        }
    }
}

/// Explicit policy knobs for pure hot/warm/cold assignment.
///
/// Scores are in basis points (`0..=1000`) to keep the policy deterministic
/// across platforms and independent of wall-clock time. Callers may use
/// [`MemoryTierInput::from_normalized_scores`] to quantize ordinary `0.0..=1.0`
/// scores at the boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryTierPolicyConfig {
    pub hot_budget: usize,
    pub warm_budget: usize,
    pub hot_score_floor: u16,
}

impl MemoryTierPolicyConfig {
    #[must_use]
    pub const fn new(hot_budget: usize, warm_budget: usize, hot_score_floor: u16) -> Self {
        Self {
            hot_budget,
            warm_budget,
            hot_score_floor,
        }
    }

    #[must_use]
    pub const fn default_swarm() -> Self {
        Self {
            hot_budget: 128,
            warm_budget: 512,
            hot_score_floor: 700,
        }
    }

    fn to_json(self) -> Value {
        json!({
            "hotBudget": self.hot_budget,
            "warmBudget": self.warm_budget,
            "hotScoreFloor": self.hot_score_floor,
        })
    }
}

/// Stable input to the memory tier policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryTierInput {
    pub memory_id: String,
    pub workspace_id: String,
    pub confidence: u16,
    pub utility: u16,
    pub importance: u16,
    pub freshness: u16,
    pub access_count: u64,
    pub reuse_count: u64,
    pub trust_class: String,
    pub explicit_query_match: bool,
    pub mandatory_provenance: bool,
    pub safety_or_failure_evidence: bool,
}

impl MemoryTierInput {
    #[must_use]
    pub fn new(memory_id: impl Into<String>, workspace_id: impl Into<String>) -> Self {
        Self {
            memory_id: memory_id.into(),
            workspace_id: workspace_id.into(),
            confidence: 0,
            utility: 0,
            importance: 0,
            freshness: 0,
            access_count: 0,
            reuse_count: 0,
            trust_class: "agent_assertion".to_owned(),
            explicit_query_match: false,
            mandatory_provenance: false,
            safety_or_failure_evidence: false,
        }
    }

    #[must_use]
    pub fn from_normalized_scores(
        memory_id: impl Into<String>,
        workspace_id: impl Into<String>,
        confidence: f64,
        utility: f64,
        importance: f64,
        freshness: f64,
    ) -> Self {
        Self {
            memory_id: memory_id.into(),
            workspace_id: workspace_id.into(),
            confidence: score_basis_points(confidence),
            utility: score_basis_points(utility),
            importance: score_basis_points(importance),
            freshness: score_basis_points(freshness),
            access_count: 0,
            reuse_count: 0,
            trust_class: "agent_assertion".to_owned(),
            explicit_query_match: false,
            mandatory_provenance: false,
            safety_or_failure_evidence: false,
        }
    }

    #[must_use]
    pub fn with_access(mut self, access_count: u64, reuse_count: u64) -> Self {
        self.access_count = access_count;
        self.reuse_count = reuse_count;
        self
    }

    #[must_use]
    pub fn with_trust_class(mut self, trust_class: impl Into<String>) -> Self {
        self.trust_class = trust_class.into();
        self
    }

    #[must_use]
    pub const fn with_explicit_query_match(mut self, explicit_query_match: bool) -> Self {
        self.explicit_query_match = explicit_query_match;
        self
    }

    #[must_use]
    pub const fn with_mandatory_provenance(mut self, mandatory_provenance: bool) -> Self {
        self.mandatory_provenance = mandatory_provenance;
        self
    }

    #[must_use]
    pub const fn with_safety_or_failure_evidence(
        mut self,
        safety_or_failure_evidence: bool,
    ) -> Self {
        self.safety_or_failure_evidence = safety_or_failure_evidence;
        self
    }

    #[must_use]
    pub fn required_evidence(&self) -> bool {
        self.explicit_query_match || self.mandatory_provenance || self.safety_or_failure_evidence
    }

    #[must_use]
    pub fn deterministic_tie_break_key(&self) -> String {
        format!("{}:{}", self.workspace_id, self.memory_id)
    }
}

/// Result of pure tier assignment for one memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryTierAssignment {
    pub memory_id: String,
    pub workspace_id: String,
    pub tier: MemoryStorageTier,
    pub tier_score: u16,
    pub tier_assignment_reason: &'static str,
    pub deterministic_tie_break_key: String,
    pub policy_version: &'static str,
    pub required_evidence_preserved: bool,
}

impl MemoryTierAssignment {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "memoryId": self.memory_id,
            "workspaceId": self.workspace_id,
            "tier": self.tier.as_str(),
            "tierScore": self.tier_score,
            "tierAssignmentReason": self.tier_assignment_reason,
            "deterministicTieBreakKey": self.deterministic_tie_break_key,
            "policyVersion": self.policy_version,
            "requiredEvidencePreserved": self.required_evidence_preserved,
        })
    }
}

/// Schema id for deterministic tier transition audit batches.
pub const MEMORY_TIER_TRANSITION_AUDIT_SCHEMA: &str = "ee.memory_tier.transition_audit.v1";

/// Previous tier state read from durable metadata before a transition pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryTierPreviousState {
    pub memory_id: String,
    pub workspace_id: String,
    pub tier: MemoryStorageTier,
    pub tier_score: u16,
    pub policy_version: String,
}

impl MemoryTierPreviousState {
    #[must_use]
    pub fn new(
        memory_id: impl Into<String>,
        workspace_id: impl Into<String>,
        tier: MemoryStorageTier,
        tier_score: u16,
        policy_version: impl Into<String>,
    ) -> Self {
        Self {
            memory_id: memory_id.into(),
            workspace_id: workspace_id.into(),
            tier,
            tier_score,
            policy_version: policy_version.into(),
        }
    }
}

/// Redaction-safe counters that explain a tier transition decision.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MemoryTierTransitionCounters {
    pub access_count: u64,
    pub reuse_count: u64,
    pub freshness_basis_points: u16,
    pub trust_basis_points: u16,
    pub decay_penalty_basis_points: u16,
}

impl MemoryTierTransitionCounters {
    #[must_use]
    pub const fn new(
        access_count: u64,
        reuse_count: u64,
        freshness_basis_points: u16,
        trust_basis_points: u16,
        decay_penalty_basis_points: u16,
    ) -> Self {
        Self {
            access_count,
            reuse_count,
            freshness_basis_points,
            trust_basis_points,
            decay_penalty_basis_points,
        }
    }

    fn to_json(self) -> Value {
        json!({
            "accessCount": self.access_count,
            "reuseCount": self.reuse_count,
            "freshnessBasisPoints": self.freshness_basis_points,
            "trustBasisPoints": self.trust_basis_points,
            "decayPenaltyBasisPoints": self.decay_penalty_basis_points,
        })
    }
}

/// Input row for the pure transition planner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryTierTransitionInput {
    pub assignment: MemoryTierAssignment,
    pub previous: Option<MemoryTierPreviousState>,
    pub counters: MemoryTierTransitionCounters,
}

impl MemoryTierTransitionInput {
    #[must_use]
    pub fn new(assignment: MemoryTierAssignment) -> Self {
        Self {
            assignment,
            previous: None,
            counters: MemoryTierTransitionCounters {
                access_count: 0,
                reuse_count: 0,
                freshness_basis_points: 0,
                trust_basis_points: 0,
                decay_penalty_basis_points: 0,
            },
        }
    }

    #[must_use]
    pub fn with_previous(mut self, previous: MemoryTierPreviousState) -> Self {
        self.previous = Some(previous);
        self
    }

    #[must_use]
    pub fn with_counters(mut self, counters: MemoryTierTransitionCounters) -> Self {
        self.counters = counters;
        self
    }
}

/// Transition kind for tier metadata. `Evict` means "move to cold metadata",
/// never tombstone or delete the memory.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MemoryTierTransitionKind {
    Admit,
    Promote,
    Retain,
    Demote,
    Evict,
}

impl MemoryTierTransitionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admit => "admit",
            Self::Promote => "promote",
            Self::Retain => "retain",
            Self::Demote => "demote",
            Self::Evict => "evict",
        }
    }
}

/// Options for a bounded transition audit batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryTierTransitionOptions {
    pub reference_time: String,
    pub dry_run: bool,
    pub max_transitions: usize,
}

impl MemoryTierTransitionOptions {
    #[must_use]
    pub fn new(reference_time: impl Into<String>) -> Self {
        Self {
            reference_time: reference_time.into(),
            dry_run: true,
            max_transitions: 0,
        }
    }

    #[must_use]
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    #[must_use]
    pub fn with_max_transitions(mut self, max_transitions: usize) -> Self {
        self.max_transitions = max_transitions;
        self
    }
}

/// One deterministic audit record for a planned tier metadata transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryTierTransitionAudit {
    pub memory_id: String,
    pub workspace_id: String,
    pub previous_tier: Option<MemoryStorageTier>,
    pub new_tier: MemoryStorageTier,
    pub previous_tier_score: Option<u16>,
    pub new_tier_score: u16,
    pub transition: MemoryTierTransitionKind,
    pub reason: &'static str,
    pub policy_version: &'static str,
    pub previous_policy_version: Option<String>,
    pub reference_time: String,
    pub deterministic_tie_break_key: String,
    pub required_evidence_preserved: bool,
    pub counters: MemoryTierTransitionCounters,
    pub dry_run: bool,
}

impl MemoryTierTransitionAudit {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "memoryId": self.memory_id,
            "workspaceId": self.workspace_id,
            "previousTier": self.previous_tier.map(MemoryStorageTier::as_str),
            "newTier": self.new_tier.as_str(),
            "previousTierScore": self.previous_tier_score,
            "newTierScore": self.new_tier_score,
            "transition": self.transition.as_str(),
            "reason": self.reason,
            "policyVersion": self.policy_version,
            "previousPolicyVersion": self.previous_policy_version,
            "referenceTime": self.reference_time,
            "deterministicTieBreakKey": self.deterministic_tie_break_key,
            "requiredEvidencePreserved": self.required_evidence_preserved,
            "sourceCounters": self.counters.to_json(),
            "dryRun": self.dry_run,
            "metadataOnly": true,
        })
    }
}

/// Pure, side-effect-free transition batch. Persistence is intentionally left
/// to callers so dry-run and write paths can share this exact audit payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryTierTransitionPlan {
    reference_time: String,
    dry_run: bool,
    max_transitions: usize,
    input_count: usize,
    audits: Vec<MemoryTierTransitionAudit>,
}

impl MemoryTierTransitionPlan {
    #[must_use]
    pub const fn schema(&self) -> &'static str {
        MEMORY_TIER_TRANSITION_AUDIT_SCHEMA
    }

    #[must_use]
    pub const fn input_count(&self) -> usize {
        self.input_count
    }

    #[must_use]
    pub fn audits(&self) -> &[MemoryTierTransitionAudit] {
        &self.audits
    }

    #[must_use]
    pub fn transition_count(&self, kind: MemoryTierTransitionKind) -> usize {
        self.audits
            .iter()
            .filter(|audit| audit.transition == kind)
            .count()
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "schema": MEMORY_TIER_TRANSITION_AUDIT_SCHEMA,
            "policyVersion": MEMORY_TIER_POLICY_VERSION,
            "referenceTime": self.reference_time,
            "dryRun": self.dry_run,
            "metadataOnly": true,
            "inputCount": self.input_count,
            "emittedCount": self.audits.len(),
            "maxTransitions": self.max_transitions,
            "transitionCounts": {
                "admit": self.transition_count(MemoryTierTransitionKind::Admit),
                "promote": self.transition_count(MemoryTierTransitionKind::Promote),
                "retain": self.transition_count(MemoryTierTransitionKind::Retain),
                "demote": self.transition_count(MemoryTierTransitionKind::Demote),
                "evict": self.transition_count(MemoryTierTransitionKind::Evict),
            },
            "audits": self
                .audits
                .iter()
                .map(MemoryTierTransitionAudit::to_json)
                .collect::<Vec<_>>(),
        })
    }
}

#[must_use]
pub fn plan_memory_tier_transitions(
    inputs: impl IntoIterator<Item = MemoryTierTransitionInput>,
    options: MemoryTierTransitionOptions,
) -> MemoryTierTransitionPlan {
    let mut audits = inputs
        .into_iter()
        .map(|input| transition_audit(input, &options))
        .collect::<Vec<_>>();
    let input_count = audits.len();
    audits.sort_by(|left, right| {
        left.deterministic_tie_break_key
            .cmp(&right.deterministic_tie_break_key)
    });
    if options.max_transitions > 0 {
        audits.truncate(options.max_transitions);
    }

    MemoryTierTransitionPlan {
        reference_time: options.reference_time,
        dry_run: options.dry_run,
        max_transitions: options.max_transitions,
        input_count,
        audits,
    }
}

fn transition_audit(
    input: MemoryTierTransitionInput,
    options: &MemoryTierTransitionOptions,
) -> MemoryTierTransitionAudit {
    let assignment = input.assignment;
    let previous = input.previous;
    let previous_tier = previous.as_ref().map(|state| state.tier);
    let previous_tier_score = previous.as_ref().map(|state| state.tier_score);
    let previous_policy_version = previous.as_ref().map(|state| state.policy_version.clone());
    let transition = transition_kind(previous_tier, assignment.tier);
    let reason = transition_reason(transition, input.counters);

    MemoryTierTransitionAudit {
        memory_id: assignment.memory_id,
        workspace_id: assignment.workspace_id,
        previous_tier,
        new_tier: assignment.tier,
        previous_tier_score,
        new_tier_score: assignment.tier_score,
        transition,
        reason,
        policy_version: assignment.policy_version,
        previous_policy_version,
        reference_time: options.reference_time.clone(),
        deterministic_tie_break_key: assignment.deterministic_tie_break_key,
        required_evidence_preserved: assignment.required_evidence_preserved,
        counters: input.counters,
        dry_run: options.dry_run,
    }
}

fn transition_kind(
    previous_tier: Option<MemoryStorageTier>,
    new_tier: MemoryStorageTier,
) -> MemoryTierTransitionKind {
    let Some(previous_tier) = previous_tier else {
        return MemoryTierTransitionKind::Admit;
    };
    if previous_tier == new_tier {
        MemoryTierTransitionKind::Retain
    } else if new_tier == MemoryStorageTier::Cold {
        MemoryTierTransitionKind::Evict
    } else if new_tier < previous_tier {
        MemoryTierTransitionKind::Promote
    } else {
        MemoryTierTransitionKind::Demote
    }
}

fn transition_reason(
    transition: MemoryTierTransitionKind,
    counters: MemoryTierTransitionCounters,
) -> &'static str {
    match transition {
        MemoryTierTransitionKind::Admit => "admit_new_tier_assignment",
        MemoryTierTransitionKind::Promote => "promote_higher_tier_score",
        MemoryTierTransitionKind::Retain => "retain_same_tier",
        MemoryTierTransitionKind::Demote if counters.decay_penalty_basis_points > 0 => {
            "demote_decay_or_trust_penalty"
        }
        MemoryTierTransitionKind::Demote => "demote_lower_tier_score",
        MemoryTierTransitionKind::Evict => "evict_to_cold_metadata_only",
    }
}

/// Assign advisory storage tiers from stable inputs.
///
/// The function is pure: it does not read config, inspect wall-clock time,
/// mutate cache state, or filter candidates. Sorting is by descending score and
/// then by a deterministic workspace/memory key.
#[must_use]
pub fn assign_memory_storage_tiers(
    inputs: impl IntoIterator<Item = MemoryTierInput>,
    config: MemoryTierPolicyConfig,
) -> Vec<MemoryTierAssignment> {
    let mut scored = inputs
        .into_iter()
        .map(|input| {
            let tier_score = memory_tier_score(&input);
            let key = input.deterministic_tie_break_key();
            (input, tier_score, key)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.2.cmp(&right.2)));

    scored
        .into_iter()
        .enumerate()
        .map(|(rank, (input, tier_score, key))| {
            let required_evidence_preserved = input.required_evidence();
            let tier = if required_evidence_preserved && tier_score < config.hot_score_floor {
                MemoryStorageTier::Cold
            } else if rank < config.hot_budget && tier_score >= config.hot_score_floor {
                MemoryStorageTier::Hot
            } else if rank < config.hot_budget.saturating_add(config.warm_budget) {
                MemoryStorageTier::Warm
            } else {
                MemoryStorageTier::Cold
            };
            MemoryTierAssignment {
                memory_id: input.memory_id,
                workspace_id: input.workspace_id,
                tier,
                tier_score,
                tier_assignment_reason: tier_assignment_reason(tier, required_evidence_preserved),
                deterministic_tie_break_key: key,
                policy_version: MEMORY_TIER_POLICY_VERSION,
                required_evidence_preserved,
            }
        })
        .collect()
}

#[must_use]
pub fn memory_storage_tier_policy_json(
    inputs: impl IntoIterator<Item = MemoryTierInput>,
    config: MemoryTierPolicyConfig,
) -> Value {
    let assignments = assign_memory_storage_tiers(inputs, config);
    json!({
        "schema": MEMORY_TIER_POLICY_SCHEMA,
        "policyVersion": MEMORY_TIER_POLICY_VERSION,
        "advisoryOnly": true,
        "config": config.to_json(),
        "assignmentCount": assignments.len(),
        "assignments": assignments
            .iter()
            .map(MemoryTierAssignment::to_json)
            .collect::<Vec<_>>(),
    })
}

fn tier_assignment_reason(
    tier: MemoryStorageTier,
    required_evidence_preserved: bool,
) -> &'static str {
    match (tier, required_evidence_preserved) {
        (MemoryStorageTier::Hot, true) => "hot_required_evidence_preserved",
        (MemoryStorageTier::Hot, false) => "hot_high_reuse_score",
        (MemoryStorageTier::Warm, true) => "warm_required_evidence_preserved",
        (MemoryStorageTier::Warm, false) => "warm_budget_admission",
        (MemoryStorageTier::Cold, true) => "cold_required_evidence_preserved",
        (MemoryStorageTier::Cold, false) => "cold_budget_overflow",
    }
}

fn memory_tier_score(input: &MemoryTierInput) -> u16 {
    let reuse = reuse_basis_points(input.access_count, input.reuse_count);
    let trust = trust_class_basis_points(&input.trust_class);
    let score = u64::from(input.confidence).saturating_mul(25)
        + u64::from(input.utility).saturating_mul(25)
        + u64::from(input.importance).saturating_mul(20)
        + u64::from(input.freshness).saturating_mul(10)
        + u64::from(trust).saturating_mul(10)
        + u64::from(reuse).saturating_mul(10);
    u16::try_from((score / 100).min(1000)).unwrap_or(1000)
}

fn score_basis_points(value: f64) -> u16 {
    if !value.is_finite() {
        return 0;
    }
    let clamped = value.clamp(0.0, 1.0);
    u16::try_from((clamped * 1000.0).floor() as u64).unwrap_or(1000)
}

fn reuse_basis_points(access_count: u64, reuse_count: u64) -> u16 {
    let weighted = access_count.saturating_add(reuse_count.saturating_mul(3));
    u16::try_from(weighted.min(100).saturating_mul(10)).unwrap_or(1000)
}

fn trust_class_basis_points(trust_class: &str) -> u16 {
    match trust_class {
        "human_explicit" => 1000,
        "peer_human_attested" => 900,
        "agent_validated" => 800,
        "cass_evidence" => 650,
        "agent_assertion" => 500,
        "legacy_import" => 300,
        _ => 400,
    }
}

/// Source class for an advisory context-hotset prewarm signal.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PrewarmSignalSource {
    Beads,
    Bv,
    AgentMail,
    VerificationBroker,
    HostProfile,
}

impl PrewarmSignalSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Beads => "beads",
            Self::Bv => "bv",
            Self::AgentMail => "agent_mail",
            Self::VerificationBroker => "verification_broker",
            Self::HostProfile => "host_profile",
        }
    }

    const fn weight(self) -> u64 {
        match self {
            Self::Beads => 48,
            Self::Bv => 44,
            Self::AgentMail => 36,
            Self::VerificationBroker => 32,
            Self::HostProfile => 20,
        }
    }
}

/// Redaction-safe input signal for advisory context hotset prewarm planning.
///
/// `summary` and `labels` may contain raw coordination text, so they are never
/// emitted by [`HotsetPrewarmPlan::to_json`]. Callers can construct these from
/// Beads, BV, Agent Mail subjects, verification blockers, or host-profile
/// posture without coupling the cache module to those services.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrewarmSignal {
    source: PrewarmSignalSource,
    stable_id: String,
    summary: String,
    labels: Vec<String>,
    priority: u8,
}

impl PrewarmSignal {
    #[must_use]
    pub fn new(
        source: PrewarmSignalSource,
        stable_id: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            source,
            stable_id: stable_id.into(),
            summary: summary.into(),
            labels: Vec::new(),
            priority: 5,
        }
    }

    #[must_use]
    pub fn with_labels(mut self, labels: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.labels = labels.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub const fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    #[must_use]
    pub const fn source(&self) -> PrewarmSignalSource {
        self.source
    }

    #[must_use]
    pub fn stable_id(&self) -> &str {
        &self.stable_id
    }
}

/// One candidate query shape predicted by the dry-run prewarm planner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotsetPrewarmCandidate {
    search_entry: SearchHotsetEntry,
    source_kinds: Vec<&'static str>,
    signal_ref_hashes: Vec<String>,
    token_count: usize,
    score: u64,
}

impl HotsetPrewarmCandidate {
    #[must_use]
    pub fn query_shape_key(&self) -> &str {
        &self.search_entry.key
    }

    #[must_use]
    pub const fn score(&self) -> u64 {
        self.score
    }

    #[must_use]
    pub const fn estimated_bytes(&self) -> usize {
        self.search_entry.estimated_bytes
    }

    #[must_use]
    pub const fn search_entry(&self) -> &SearchHotsetEntry {
        &self.search_entry
    }

    fn to_json(&self) -> Value {
        json!({
            "queryShapeKey": &self.search_entry.key,
            "kind": self.search_entry.kind.as_str(),
            "generation": self.search_entry.generation,
            "sourceKinds": &self.source_kinds,
            "signalRefHashes": &self.signal_ref_hashes,
            "tokenCount": self.token_count,
            "score": self.score,
            "estimatedBytes": self.search_entry.estimated_bytes,
            "redactionStatus": PREWARM_REDACTION_STATUS,
        })
    }
}

#[derive(Clone, Debug)]
struct PrewarmCandidateAccumulator {
    entry: SearchHotsetEntry,
    source_kinds: BTreeSet<&'static str>,
    signal_ref_hashes: BTreeSet<String>,
    token_count: usize,
    score: u64,
}

impl PrewarmCandidateAccumulator {
    fn new(entry: SearchHotsetEntry, signal: &PrewarmSignal, token_count: usize) -> Self {
        let mut source_kinds = BTreeSet::new();
        source_kinds.insert(signal.source.as_str());
        let mut signal_ref_hashes = BTreeSet::new();
        signal_ref_hashes.insert(signal_ref_hash(signal));
        Self {
            entry,
            source_kinds,
            signal_ref_hashes,
            token_count,
            score: prewarm_signal_score(signal, token_count),
        }
    }

    fn merge(&mut self, entry: SearchHotsetEntry, signal: &PrewarmSignal, token_count: usize) {
        self.entry.hit_count = self.entry.hit_count.saturating_add(entry.hit_count);
        self.entry.estimated_bytes = self.entry.estimated_bytes.max(entry.estimated_bytes);
        self.entry.generation = self.entry.generation.max(entry.generation);
        self.source_kinds.insert(signal.source.as_str());
        self.signal_ref_hashes.insert(signal_ref_hash(signal));
        self.token_count = self.token_count.max(token_count);
        self.score = self
            .score
            .saturating_add(prewarm_signal_score(signal, token_count));
    }

    fn into_candidate(self) -> HotsetPrewarmCandidate {
        HotsetPrewarmCandidate {
            search_entry: self.entry,
            source_kinds: self.source_kinds.into_iter().collect(),
            signal_ref_hashes: self.signal_ref_hashes.into_iter().collect(),
            token_count: self.token_count,
            score: self.score,
        }
    }
}

/// Advisory, side-effect-free context hotset prewarm plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotsetPrewarmPlan {
    generation: u64,
    budget: HotsetBudget,
    input_signal_count: usize,
    skipped_signal_count: usize,
    max_candidates: usize,
    candidates: Vec<HotsetPrewarmCandidate>,
}

impl HotsetPrewarmPlan {
    #[must_use]
    pub const fn schema(&self) -> &'static str {
        PREWARM_PLAN_SCHEMA
    }

    #[must_use]
    pub fn candidates(&self) -> &[HotsetPrewarmCandidate] {
        &self.candidates
    }

    #[must_use]
    pub const fn input_signal_count(&self) -> usize {
        self.input_signal_count
    }

    #[must_use]
    pub const fn skipped_signal_count(&self) -> usize {
        self.skipped_signal_count
    }

    #[must_use]
    pub fn estimated_memory_bytes(&self) -> usize {
        self.candidates
            .iter()
            .map(HotsetPrewarmCandidate::estimated_bytes)
            .sum()
    }

    #[must_use]
    pub fn expected_latency_win_ms(&self) -> u64 {
        self.candidates
            .iter()
            .map(|candidate| {
                8_u64
                    .saturating_add(candidate.search_entry.hit_count.min(8))
                    .saturating_add((candidate.score / 32).min(16))
            })
            .sum()
    }

    #[must_use]
    pub fn degraded_codes(&self) -> Vec<Value> {
        let mut degraded = Vec::new();
        if self.candidates.is_empty() {
            degraded.push(json!({
                "code": PREWARM_NO_SIGNAL_CODE,
                "severity": "low",
                "message": "No usable Beads, BV, Agent Mail, verification, or host-profile signals were available for context hotset prewarm.",
                "repair": "Capture at least one current coordination signal before running prewarm.",
                "details": {
                    "inputSignalCount": self.input_signal_count,
                    "skippedSignalCount": self.skipped_signal_count,
                }
            }));
        }
        degraded
    }

    #[must_use]
    pub fn search_hotset_entries(&self) -> Vec<SearchHotsetEntry> {
        self.candidates
            .iter()
            .map(|candidate| candidate.search_entry.clone())
            .collect()
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        let remaining_entries = self
            .budget
            .max_entries
            .saturating_sub(self.budget.current_entries);
        let remaining_bytes = self
            .budget
            .max_bytes
            .saturating_sub(self.budget.current_bytes);
        let estimated_bytes = self.estimated_memory_bytes();
        let cache_status = if self.budget.max_entries == 0 && self.budget.max_bytes == 0 {
            "unbudgeted"
        } else if self.candidates.len() <= remaining_entries && estimated_bytes <= remaining_bytes {
            "admissible"
        } else {
            "over_budget"
        };

        json!({
            "schema": PREWARM_PLAN_SCHEMA,
            "generation": self.generation,
            "redactionStatus": PREWARM_REDACTION_STATUS,
            "inputSignalCount": self.input_signal_count,
            "skippedSignalCount": self.skipped_signal_count,
            "candidateCount": self.candidates.len(),
            "maxCandidates": self.max_candidates,
            "estimatedMemoryBytes": estimated_bytes,
            "expectedLatencyWinMs": self.expected_latency_win_ms(),
            "indexPosture": {
                "status": if self.candidates.is_empty() { "cold" } else { "prewarm_recommended" },
                "generation": self.generation,
            },
            "graphPosture": {
                "status": "not_required_for_dry_run",
            },
            "cachePosture": {
                "status": cache_status,
                "remainingEntries": remaining_entries,
                "remainingBytes": remaining_bytes,
            },
            "admissionBudget": self.budget.to_json(),
            "searchEntries": self
                .candidates
                .iter()
                .map(|candidate| candidate.search_entry.data_json())
                .collect::<Vec<_>>(),
            "candidates": self
                .candidates
                .iter()
                .map(HotsetPrewarmCandidate::to_json)
                .collect::<Vec<_>>(),
            "degraded": self.degraded_codes(),
        })
    }
}

/// Predict a bounded, redaction-safe set of query shapes for `ee context`
/// prewarm. This function is pure and advisory: it does not read Beads, BV,
/// Agent Mail, caches, files, or databases, and it does not mutate derived
/// state. Callers pass already-captured coordination summaries.
#[must_use]
pub fn plan_context_hotset_prewarm(
    signals: impl IntoIterator<Item = PrewarmSignal>,
    generation: u64,
    budget: HotsetBudget,
    max_candidates: usize,
) -> HotsetPrewarmPlan {
    let mut input_signal_count = 0_usize;
    let mut skipped_signal_count = 0_usize;
    let mut merged: BTreeMap<String, PrewarmCandidateAccumulator> = BTreeMap::new();

    for signal in signals {
        input_signal_count = input_signal_count.saturating_add(1);
        let tokens = prewarm_signal_tokens(&signal);
        if tokens.is_empty() {
            skipped_signal_count = skipped_signal_count.saturating_add(1);
            continue;
        }
        let query_shape = tokens.join(" ");
        let Some(entry) = SearchHotsetEntry::query_shape(&query_shape, generation, 1) else {
            skipped_signal_count = skipped_signal_count.saturating_add(1);
            continue;
        };
        let key = entry.key.clone();
        if let Some(existing) = merged.get_mut(&key) {
            existing.merge(entry, &signal, tokens.len());
        } else {
            merged.insert(
                key,
                PrewarmCandidateAccumulator::new(entry, &signal, tokens.len()),
            );
        }
    }

    let mut candidates: Vec<_> = merged
        .into_values()
        .map(PrewarmCandidateAccumulator::into_candidate)
        .collect();
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.query_shape_key().cmp(right.query_shape_key()))
    });
    if max_candidates > 0 {
        candidates.truncate(max_candidates);
    }

    HotsetPrewarmPlan {
        generation,
        budget,
        input_signal_count,
        skipped_signal_count,
        max_candidates,
        candidates,
    }
}

/// Options for the explicit, side-effect-free `ee cache prewarm` report.
#[derive(Clone, Debug, PartialEq)]
pub struct CachePrewarmOptions {
    pub profile: String,
    pub budget: CacheBudget,
    pub current_generation: Option<u64>,
    pub allow_stale_hotset: bool,
}

impl CachePrewarmOptions {
    #[must_use]
    pub fn new(profile: impl Into<String>, budget: CacheBudget) -> Self {
        Self {
            profile: profile.into(),
            budget,
            current_generation: None,
            allow_stale_hotset: false,
        }
    }

    #[must_use]
    pub const fn with_current_generation(mut self, current_generation: Option<u64>) -> Self {
        self.current_generation = current_generation;
        self
    }

    #[must_use]
    pub const fn with_allow_stale_hotset(mut self, allow_stale_hotset: bool) -> Self {
        self.allow_stale_hotset = allow_stale_hotset;
        self
    }
}

/// Build the canonical `ee.cache.prewarm.v1` report from a redaction-safe
/// `ee.cache.hotset.v1` manifest. The function only reads the supplied JSON and
/// returns an admission report; cache mutation is left to a future derived-asset
/// writer once that writer can provide its own audit trail.
pub fn cache_prewarm_report_from_manifest_json(
    manifest: &Value,
    options: &CachePrewarmOptions,
) -> Result<Value, String> {
    ensure_manifest_header(manifest)?;

    let workspace_id = string_field(manifest, "workspaceId")?.to_owned();
    let workspace_generation = u64_field(manifest, "workspaceGeneration")?;
    let index_generation = u64_field(manifest, "indexGeneration")?;
    let admission_threshold = u64_field(manifest, "admissionThreshold")?;
    let manifest_profile = manifest
        .get("profileTier")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let mut search_entries = parse_search_entries(manifest.get("searchEntries"))?;
    let mut pack_entries = parse_pack_entries(manifest.get("packEntries"))?;
    let requested_search_entries = search_entries.len();
    let requested_pack_entries = pack_entries.len();
    let requested_total = requested_search_entries.saturating_add(requested_pack_entries);

    let requested_generation = options.current_generation.unwrap_or(admission_threshold);
    let stale_hotset_admitted = options.allow_stale_hotset
        && (entries_have_generation_mismatch(&search_entries, requested_generation, |entry| {
            entry.generation
        }) || entries_have_generation_mismatch(&pack_entries, requested_generation, |entry| {
            entry.generation
        }));
    if options.allow_stale_hotset {
        normalize_search_entry_generations(&mut search_entries, requested_generation);
        normalize_pack_entry_generations(&mut pack_entries, requested_generation);
    }

    let search_report = prewarm_search_hotset(
        &SearchHotset::new(search_entries),
        SearchCacheGovernor::new(requested_generation, options.budget).with_current_usage(0, 0),
    )
    .data_json();
    let pack_report = prewarm_pack_hotset(
        &PackHotset::new(pack_entries),
        PackCacheGovernor::new(requested_generation, options.budget).with_current_usage(0, 0),
    )
    .data_json();

    let admitted_search_entries = usize_json_field(&search_report, "admittedEntries");
    let admitted_pack_entries = usize_json_field(&pack_report, "admittedEntries");
    let admitted_total = admitted_search_entries.saturating_add(admitted_pack_entries);
    let rejected_search_entries = usize_json_field(&search_report, "rejectedEntries");
    let rejected_pack_entries = usize_json_field(&pack_report, "rejectedEntries");
    let rejected_total = rejected_search_entries.saturating_add(rejected_pack_entries);

    let degraded = cache_prewarm_degraded(
        requested_total,
        &search_report,
        &pack_report,
        stale_hotset_admitted,
        requested_generation,
        admission_threshold,
    );
    let latency = cache_prewarm_latency_estimate(&search_report, &pack_report);
    let memory_pressure = max_report_pressure(&search_report, &pack_report).as_str();

    Ok(json!({
        "schema": CACHE_PREWARM_SCHEMA,
        "sourceSchema": SCHEMA,
        "profile": options.profile.as_str(),
        "allowStaleHotset": options.allow_stale_hotset,
        "fromHotset": {
            "workspaceId": workspace_id,
            "workspaceGeneration": workspace_generation,
            "indexGeneration": index_generation,
            "admissionThreshold": admission_threshold,
            "profileTier": manifest_profile,
            "redactionStatus": REDACTION_STATUS,
        },
        "requested": {
            "searchEntries": requested_search_entries,
            "packEntries": requested_pack_entries,
            "totalEntries": requested_total,
        },
        "admitted": {
            "searchEntries": admitted_search_entries,
            "packEntries": admitted_pack_entries,
            "totalEntries": admitted_total,
        },
        "rejected": {
            "searchEntries": rejected_search_entries,
            "packEntries": rejected_pack_entries,
            "totalEntries": rejected_total,
        },
        "budgetSource": format!("profile:{}", options.profile),
        "memoryPressure": memory_pressure,
        "latencyEstimate": latency,
        "redactionSafety": {
            "status": "safe",
            "summary": "query_hashes_and_cache_keys_only",
            "rawContentStored": false,
        },
        "reports": {
            "search": search_report,
            "pack": pack_report,
        },
        "degraded": degraded,
    }))
}

/// Build a cache-prewarm report and attach advisory memory-tier residency
/// posture. The tier metadata is treated as a derived input: stale tier
/// generations are rejected and surfaced through `degraded[]`, while the
/// underlying search/pack prewarm report remains computed from the hotset
/// manifest alone.
pub fn tier_aware_cache_prewarm_report_from_manifest_json(
    manifest: &Value,
    tier_assignments: impl IntoIterator<Item = MemoryTierAssignment>,
    tier_generation: u64,
    options: &CachePrewarmOptions,
) -> Result<Value, String> {
    ensure_manifest_header(manifest)?;
    let admission_threshold = u64_field(manifest, "admissionThreshold")?;
    let current_generation = options.current_generation.unwrap_or(admission_threshold);
    let assignments = tier_assignments.into_iter().collect::<Vec<_>>();

    let mut report = cache_prewarm_report_from_manifest_json(manifest, options)?;
    let (posture, degraded) =
        memory_tier_prewarm_posture(&report, &assignments, tier_generation, current_generation);
    let Some(object) = report.as_object_mut() else {
        return Err("cache prewarm report must be a JSON object".to_owned());
    };
    object.insert("memoryTierPosture".to_owned(), posture);
    if !degraded.is_empty() {
        match object.get_mut("degraded") {
            Some(Value::Array(existing)) => existing.extend(degraded),
            _ => {
                object.insert("degraded".to_owned(), Value::Array(degraded));
            }
        }
    }
    Ok(report)
}

fn ensure_manifest_header(manifest: &Value) -> Result<(), String> {
    if manifest.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!("expected {SCHEMA} manifest"));
    }
    if manifest.get("redactionStatus").and_then(Value::as_str) != Some(REDACTION_STATUS) {
        return Err(format!(
            "hotset manifest must use {REDACTION_STATUS} redaction status"
        ));
    }
    Ok(())
}

fn parse_search_entries(value: Option<&Value>) -> Result<Vec<SearchHotsetEntry>, String> {
    let Some(Value::Array(entries)) = value else {
        return Ok(Vec::new());
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| parse_search_entry(entry, index))
        .collect()
}

fn parse_search_entry(value: &Value, index: usize) -> Result<SearchHotsetEntry, String> {
    if string_field(value, "redactionStatus")? != REDACTION_STATUS {
        return Err(format!(
            "searchEntries[{index}] must use {REDACTION_STATUS} redaction status"
        ));
    }
    Ok(SearchHotsetEntry {
        key: string_field(value, "key")?.to_owned(),
        kind: parse_search_kind(string_field(value, "kind")?)
            .ok_or_else(|| format!("searchEntries[{index}] has unknown kind"))?,
        generation: u64_field(value, "generation")?,
        estimated_bytes: usize_field(value, "estimatedBytes")?,
        hit_count: u64_field(value, "hitCount")?,
        redaction_status: REDACTION_STATUS,
    })
}

fn parse_pack_entries(value: Option<&Value>) -> Result<Vec<PackHotsetEntry>, String> {
    let Some(Value::Array(entries)) = value else {
        return Ok(Vec::new());
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| parse_pack_entry(entry, index))
        .collect()
}

fn parse_pack_entry(value: &Value, index: usize) -> Result<PackHotsetEntry, String> {
    if string_field(value, "redactionStatus")? != REDACTION_STATUS {
        return Err(format!(
            "packEntries[{index}] must use {REDACTION_STATUS} redaction status"
        ));
    }
    let kind = parse_pack_kind(string_field(value, "kind")?)
        .ok_or_else(|| format!("packEntries[{index}] has unknown kind"))?;
    let section = match value.get("section").and_then(Value::as_str) {
        Some(raw) => Some(
            parse_pack_section(raw)
                .ok_or_else(|| format!("packEntries[{index}] has unknown section"))?,
        ),
        None => None,
    };
    if kind == PackHotsetEntryKind::PackSection && section.is_none() {
        return Err(format!(
            "packEntries[{index}] pack_section requires section"
        ));
    }
    Ok(PackHotsetEntry {
        key: string_field(value, "key")?.to_owned(),
        kind,
        section,
        generation: u64_field(value, "generation")?,
        estimated_bytes: usize_field(value, "estimatedBytes")?,
        hit_count: u64_field(value, "hitCount")?,
        redaction_status: REDACTION_STATUS,
    })
}

fn parse_search_kind(raw: &str) -> Option<SearchHotsetEntryKind> {
    match raw {
        "memory" => Some(SearchHotsetEntryKind::Memory),
        "query_shape" => Some(SearchHotsetEntryKind::QueryShape),
        "search_document" => Some(SearchHotsetEntryKind::SearchDocument),
        "graph_neighborhood" => Some(SearchHotsetEntryKind::GraphNeighborhood),
        _ => None,
    }
}

fn parse_pack_kind(raw: &str) -> Option<PackHotsetEntryKind> {
    match raw {
        "pack_section" => Some(PackHotsetEntryKind::PackSection),
        "selection_audit" => Some(PackHotsetEntryKind::SelectionAudit),
        _ => None,
    }
}

fn parse_pack_section(raw: &str) -> Option<PackSection> {
    match raw {
        "procedural_rules" => Some(PackSection::ProceduralRules),
        "decisions" => Some(PackSection::Decisions),
        "failures" => Some(PackSection::Failures),
        "evidence" => Some(PackSection::Evidence),
        "artifacts" => Some(PackSection::Artifacts),
        _ => None,
    }
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field {field}"))
}

fn u64_field(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing integer field {field}"))
}

fn usize_field(value: &Value, field: &str) -> Result<usize, String> {
    let raw = u64_field(value, field)?;
    usize::try_from(raw).map_err(|_| format!("field {field} exceeds usize"))
}

fn usize_json_field(value: &Value, field: &str) -> usize {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|raw| usize::try_from(raw).ok())
        .unwrap_or(0)
}

fn entries_have_generation_mismatch<T>(
    entries: &[T],
    requested_generation: u64,
    generation: impl Fn(&T) -> u64,
) -> bool {
    entries
        .iter()
        .any(|entry| generation(entry) != requested_generation)
}

fn normalize_search_entry_generations(entries: &mut [SearchHotsetEntry], generation: u64) {
    for entry in entries {
        entry.generation = generation;
    }
}

fn normalize_pack_entry_generations(entries: &mut [PackHotsetEntry], generation: u64) {
    for entry in entries {
        entry.generation = generation;
    }
}

fn cache_prewarm_degraded(
    requested_total: usize,
    search_report: &Value,
    pack_report: &Value,
    stale_hotset_admitted: bool,
    requested_generation: u64,
    admission_threshold: u64,
) -> Vec<Value> {
    let mut degraded = Vec::new();
    if requested_total == 0 {
        degraded.push(json!({
            "code": PREWARM_NO_SIGNAL_CODE,
            "severity": "low",
            "message": "Hotset manifest contains no usable search or pack entries to prewarm.",
            "repair": "Capture a current hotset manifest before running cache prewarm.",
            "details": {
                "requestedEntries": 0,
            }
        }));
    }
    let stale_rejected = report_status(search_report) == Some("stale_generation")
        || report_status(pack_report) == Some("stale_generation");
    if stale_rejected {
        degraded.push(json!({
            "code": STALE_HOTSET_CODE,
            "severity": "medium",
            "message": "Cache prewarm rejected the hotset because its generation does not match the current generation.",
            "repair": "Recapture the hotset or rerun with --allow-stale-hotset when stale warming is intentional.",
            "details": {
                "requestedGeneration": requested_generation,
                "admissionThreshold": admission_threshold,
            }
        }));
    } else if stale_hotset_admitted {
        degraded.push(json!({
            "code": STALE_HOTSET_CODE,
            "severity": "medium",
            "message": "Cache prewarm admitted a stale hotset because --allow-stale-hotset was supplied.",
            "repair": "Recapture the hotset against the current workspace and index generation when precision matters.",
            "details": {
                "requestedGeneration": requested_generation,
                "admissionThreshold": admission_threshold,
                "allowStaleHotset": true,
            }
        }));
    }
    degraded
}

fn memory_tier_prewarm_posture(
    report: &Value,
    assignments: &[MemoryTierAssignment],
    tier_generation: u64,
    current_generation: u64,
) -> (Value, Vec<Value>) {
    let mut sorted_assignments = assignments.iter().collect::<Vec<_>>();
    sorted_assignments.sort_by(|left, right| {
        memory_tier_assignment_key(left)
            .cmp(&memory_tier_assignment_key(right))
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });

    if sorted_assignments.is_empty() {
        return (
            memory_tier_posture_json(
                "empty",
                tier_generation,
                current_generation,
                MemoryTierPrewarmCounts::default(),
            ),
            Vec::new(),
        );
    }

    if tier_generation < current_generation {
        let counts = MemoryTierPrewarmCounts {
            stale_tier_rejected_count: sorted_assignments.len(),
            required_cold_evidence_count: sorted_assignments
                .iter()
                .filter(|assignment| {
                    assignment.tier == MemoryStorageTier::Cold
                        && assignment.required_evidence_preserved
                })
                .count(),
            ..MemoryTierPrewarmCounts::default()
        };
        let degraded = vec![json!({
            "code": MEMORY_TIER_METADATA_STALE_CODE,
            "severity": "medium",
            "message": "Cache prewarm rejected memory tier metadata because the tier generation is older than the current generation.",
            "repair": "Regenerate memory tier assignments before running tier-aware prewarm.",
            "details": {
                "tierGeneration": tier_generation,
                "currentGeneration": current_generation,
                "staleTierRejectedCount": counts.stale_tier_rejected_count,
            }
        })];
        return (
            memory_tier_posture_json(
                "stale_rejected",
                tier_generation,
                current_generation,
                counts,
            ),
            degraded,
        );
    }

    let admitted_memory_keys = admitted_search_memory_keys(report);
    let mut counts = MemoryTierPrewarmCounts::default();
    for assignment in sorted_assignments {
        let key = memory_tier_assignment_key(assignment);
        let admitted = admitted_memory_keys.contains(&key);
        match (assignment.tier, admitted) {
            (MemoryStorageTier::Hot, true) => counts.admitted_hot_count += 1,
            (MemoryStorageTier::Warm, true) => counts.admitted_warm_count += 1,
            (MemoryStorageTier::Cold, true) => counts.admitted_cold_count += 1,
            (MemoryStorageTier::Cold, false) => counts.cold_recall_skipped_count += 1,
            (MemoryStorageTier::Hot | MemoryStorageTier::Warm, false) => {}
        }
        if assignment.tier == MemoryStorageTier::Cold && assignment.required_evidence_preserved {
            counts.required_cold_evidence_count += 1;
        }
    }

    (
        memory_tier_posture_json("fresh", tier_generation, current_generation, counts),
        Vec::new(),
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MemoryTierPrewarmCounts {
    admitted_hot_count: usize,
    admitted_warm_count: usize,
    admitted_cold_count: usize,
    cold_recall_skipped_count: usize,
    required_cold_evidence_count: usize,
    stale_tier_rejected_count: usize,
}

fn memory_tier_posture_json(
    status: &'static str,
    tier_generation: u64,
    current_generation: u64,
    counts: MemoryTierPrewarmCounts,
) -> Value {
    json!({
        "status": status,
        "advisoryOnly": true,
        "policyVersion": MEMORY_TIER_POLICY_VERSION,
        "tierGeneration": tier_generation,
        "currentGeneration": current_generation,
        "preservesColdRecallEligibility": true,
        "admittedHotCount": counts.admitted_hot_count,
        "admittedWarmCount": counts.admitted_warm_count,
        "admittedColdCount": counts.admitted_cold_count,
        "coldRecallSkippedCount": counts.cold_recall_skipped_count,
        "requiredColdEvidenceCount": counts.required_cold_evidence_count,
        "staleTierRejectedCount": counts.stale_tier_rejected_count,
    })
}

fn admitted_search_memory_keys(report: &Value) -> BTreeSet<String> {
    report
        .pointer("/reports/search/admitted")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("kind").and_then(Value::as_str) == Some("memory"))
        .filter_map(|entry| entry.get("key").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

fn memory_tier_assignment_key(assignment: &MemoryTierAssignment) -> String {
    SearchHotsetEntry::memory(&assignment.memory_id, 0, 0).key
}

fn report_status(report: &Value) -> Option<&str> {
    report.get("status").and_then(Value::as_str)
}

fn cache_prewarm_latency_estimate(search_report: &Value, pack_report: &Value) -> Value {
    let mut estimated_components = Vec::new();
    let mut unmeasured_components = Vec::new();
    let (search_cold, search_warm) = match latency_fields(search_report) {
        Some(latency) => {
            estimated_components.push("search");
            latency
        }
        None => {
            if search_report.get("prewarmEvidence").is_some() {
                unmeasured_components.push(json!({
                    "component": "search",
                    "reason": "search_prewarm_reports_admission_stats_not_latency",
                }));
            }
            (0, 0)
        }
    };
    let (pack_cold, pack_warm) = match latency_fields(pack_report) {
        Some(latency) => {
            estimated_components.push("pack");
            latency
        }
        None => {
            if pack_report.get("prewarmEvidence").is_some() {
                unmeasured_components.push(json!({
                    "component": "pack",
                    "reason": "pack_prewarm_reports_admission_stats_not_latency",
                }));
            }
            (0, 0)
        }
    };
    let cold = search_cold.saturating_add(pack_cold);
    let warm = search_warm.saturating_add(pack_warm);
    let win = cold.saturating_sub(warm);
    let ratio = if cold == 0 {
        0.0
    } else {
        ((win as f64 / cold as f64) * 10_000.0).round() / 10_000.0
    };
    json!({
        "coldLatencyUs": cold,
        "warmLatencyUs": warm,
        "expectedWinUs": win,
        "expectedWinMs": win / 1_000,
        "latencyWinRatio": ratio,
        "estimatedComponents": estimated_components,
        "unmeasuredComponents": unmeasured_components,
    })
}

fn latency_fields(report: &Value) -> Option<(u64, u64)> {
    let benchmark = report.get("benchmarkEvidence")?;
    let cold = benchmark.get("coldLatencyUs").and_then(Value::as_u64)?;
    let warm = benchmark.get("warmLatencyUs").and_then(Value::as_u64)?;
    Some((cold, warm))
}

fn max_report_pressure(search_report: &Value, pack_report: &Value) -> MemoryPressure {
    pressure_from_report(search_report).max(pressure_from_report(pack_report))
}

fn pressure_from_report(report: &Value) -> MemoryPressure {
    match report.get("memoryPressure").and_then(Value::as_str) {
        Some("critical") => MemoryPressure::Critical,
        Some("high") => MemoryPressure::High,
        _ => MemoryPressure::Normal,
    }
}

fn prewarm_signal_tokens(signal: &PrewarmSignal) -> Vec<String> {
    let mut tokens = Vec::new();
    collect_prewarm_tokens(&signal.summary, &mut tokens);
    for label in &signal.labels {
        collect_prewarm_tokens(label, &mut tokens);
    }
    tokens.sort();
    tokens.dedup();
    tokens.truncate(12);
    tokens
}

fn collect_prewarm_tokens(input: &str, tokens: &mut Vec<String>) {
    let mut token = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            token.push(ch.to_ascii_lowercase());
            if token.len() >= 48 {
                finish_prewarm_token(&mut token, tokens);
            }
        } else {
            finish_prewarm_token(&mut token, tokens);
        }
    }
    finish_prewarm_token(&mut token, tokens);
}

fn finish_prewarm_token(token: &mut String, tokens: &mut Vec<String>) {
    if token.len() >= 2 {
        tokens.push(std::mem::take(token));
    } else {
        token.clear();
    }
}

fn prewarm_signal_score(signal: &PrewarmSignal, token_count: usize) -> u64 {
    let priority = signal.priority.min(9);
    let priority_weight = u64::from(10_u8.saturating_sub(priority)).saturating_mul(8);
    let token_weight = token_count.min(12) as u64;
    signal
        .source
        .weight()
        .saturating_add(priority_weight)
        .saturating_add(token_weight)
}

fn signal_ref_hash(signal: &PrewarmSignal) -> String {
    let digest_input = format!("{}:{}", signal.source.as_str(), signal.stable_id);
    format!("blake3:{}", blake3::hash(digest_input.as_bytes()).to_hex())
}

/// Builder for [`HotsetManifest`]. The builder owns the deterministic merge
/// and stale-classification pipeline; the resulting manifest is immutable.
#[derive(Clone, Debug)]
pub struct HotsetManifestBuilder {
    workspace_id: String,
    gate: GenerationGate,
    profile_tier: Option<String>,
    captured_at: Option<String>,
    search_entries: Vec<SearchHotsetEntry>,
    pack_entries: Vec<PackHotsetEntry>,
    budget: HotsetBudget,
}

impl HotsetManifestBuilder {
    #[must_use]
    pub fn new(workspace_id: impl Into<String>, gate: GenerationGate) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            gate,
            profile_tier: None,
            captured_at: None,
            search_entries: Vec::new(),
            pack_entries: Vec::new(),
            budget: HotsetBudget::default(),
        }
    }

    #[must_use]
    pub fn with_profile_tier(mut self, profile_tier: impl Into<String>) -> Self {
        self.profile_tier = Some(profile_tier.into());
        self
    }

    /// Set the volatile `capturedAt` value. Callers that want byte-identical
    /// JSON across runs should either omit this or strip the field after
    /// serialization. Keeping it optional means the determinism test does
    /// not need a clock fake.
    #[must_use]
    pub fn with_captured_at(mut self, captured_at: impl Into<String>) -> Self {
        self.captured_at = Some(captured_at.into());
        self
    }

    #[must_use]
    pub fn with_budget(mut self, budget: HotsetBudget) -> Self {
        self.budget = budget;
        self
    }

    #[must_use]
    pub fn search_entries(mut self, entries: impl IntoIterator<Item = SearchHotsetEntry>) -> Self {
        self.search_entries.extend(entries);
        self
    }

    #[must_use]
    pub fn pack_entries(mut self, entries: impl IntoIterator<Item = PackHotsetEntry>) -> Self {
        self.pack_entries.extend(entries);
        self
    }

    #[must_use]
    pub fn build(self) -> HotsetManifest {
        let threshold = self.gate.admission_threshold();

        let (search_admitted, search_rejected_stale) =
            partition_search_entries(self.search_entries, threshold);
        let (pack_admitted, pack_rejected_stale) =
            partition_pack_entries(self.pack_entries, threshold);

        HotsetManifest {
            workspace_id: self.workspace_id,
            gate: self.gate,
            profile_tier: self.profile_tier,
            captured_at: self.captured_at,
            budget: self.budget,
            search_admitted,
            search_rejected_stale,
            pack_admitted,
            pack_rejected_stale,
        }
    }
}

fn partition_search_entries(
    entries: Vec<SearchHotsetEntry>,
    threshold: u64,
) -> (Vec<SearchHotsetEntry>, Vec<SearchHotsetEntry>) {
    let mut admitted: BTreeMap<(SearchHotsetEntryKind, String), SearchHotsetEntry> =
        BTreeMap::new();
    let mut rejected: BTreeMap<(SearchHotsetEntryKind, String), SearchHotsetEntry> =
        BTreeMap::new();
    for entry in entries {
        if entry.generation >= threshold {
            merge_search_entry(&mut admitted, entry);
        } else {
            merge_search_entry(&mut rejected, entry);
        }
    }
    (
        admitted.into_values().collect(),
        rejected.into_values().collect(),
    )
}

fn merge_search_entry(
    entries: &mut BTreeMap<(SearchHotsetEntryKind, String), SearchHotsetEntry>,
    entry: SearchHotsetEntry,
) {
    let key = (entry.kind, entry.key.clone());
    entries
        .entry(key)
        .and_modify(|existing| {
            existing.hit_count = existing.hit_count.saturating_add(entry.hit_count);
            existing.estimated_bytes = existing.estimated_bytes.max(entry.estimated_bytes);
            existing.generation = existing.generation.max(entry.generation);
        })
        .or_insert(entry);
}

fn partition_pack_entries(
    entries: Vec<PackHotsetEntry>,
    threshold: u64,
) -> (Vec<PackHotsetEntry>, Vec<PackHotsetEntry>) {
    let mut admitted: BTreeMap<(PackHotsetEntryKind, String), PackHotsetEntry> = BTreeMap::new();
    let mut rejected: BTreeMap<(PackHotsetEntryKind, String), PackHotsetEntry> = BTreeMap::new();
    for entry in entries {
        if entry.generation >= threshold {
            merge_pack_entry(&mut admitted, entry);
        } else {
            merge_pack_entry(&mut rejected, entry);
        }
    }
    (
        admitted.into_values().collect(),
        rejected.into_values().collect(),
    )
}

fn merge_pack_entry(
    entries: &mut BTreeMap<(PackHotsetEntryKind, String), PackHotsetEntry>,
    entry: PackHotsetEntry,
) {
    let key = (entry.kind, entry.key.clone());
    entries
        .entry(key)
        .and_modify(|existing| {
            existing.hit_count = existing.hit_count.saturating_add(entry.hit_count);
            existing.estimated_bytes = existing.estimated_bytes.max(entry.estimated_bytes);
            existing.generation = existing.generation.max(entry.generation);
        })
        .or_insert(entry);
}

/// Immutable hotset manifest produced by [`HotsetManifestBuilder`].
#[derive(Clone, Debug)]
pub struct HotsetManifest {
    workspace_id: String,
    gate: GenerationGate,
    profile_tier: Option<String>,
    captured_at: Option<String>,
    budget: HotsetBudget,
    search_admitted: Vec<SearchHotsetEntry>,
    search_rejected_stale: Vec<SearchHotsetEntry>,
    pack_admitted: Vec<PackHotsetEntry>,
    pack_rejected_stale: Vec<PackHotsetEntry>,
}

impl HotsetManifest {
    #[must_use]
    pub const fn schema(&self) -> &'static str {
        SCHEMA
    }

    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    #[must_use]
    pub const fn gate(&self) -> GenerationGate {
        self.gate
    }

    #[must_use]
    pub fn profile_tier(&self) -> Option<&str> {
        self.profile_tier.as_deref()
    }

    #[must_use]
    pub fn captured_at(&self) -> Option<&str> {
        self.captured_at.as_deref()
    }

    #[must_use]
    pub const fn budget(&self) -> HotsetBudget {
        self.budget
    }

    #[must_use]
    pub fn candidate_count(&self) -> usize {
        self.admitted_count() + self.rejected_stale_count()
    }

    #[must_use]
    pub fn admitted_count(&self) -> usize {
        self.search_admitted.len() + self.pack_admitted.len()
    }

    #[must_use]
    pub fn rejected_stale_count(&self) -> usize {
        self.search_rejected_stale.len() + self.pack_rejected_stale.len()
    }

    /// True when every entry in the manifest (admitted or rejected) carries
    /// the expected `content_not_stored` redaction marker.
    #[must_use]
    pub fn is_redaction_safe(&self) -> bool {
        let search_safe = self
            .search_admitted
            .iter()
            .chain(self.search_rejected_stale.iter())
            .all(SearchHotsetEntry::is_redaction_safe);
        let pack_safe = self
            .pack_admitted
            .iter()
            .chain(self.pack_rejected_stale.iter())
            .all(PackHotsetEntry::is_redaction_safe);
        search_safe && pack_safe
    }

    /// The single degraded code this surface emits today. Returns an empty
    /// vec when nothing degraded.
    #[must_use]
    pub fn degraded_codes(&self) -> Vec<Value> {
        let mut codes = Vec::new();
        let rejected = self.rejected_stale_count();
        if rejected > 0 {
            codes.push(json!({
                "code": STALE_HOTSET_CODE,
                "severity": "medium",
                "message": format!(
                    "Hotset rejected {rejected} entries older than the current generation; \
                     warming would degrade pack quality."
                ),
                "repair": "Recapture the hotset against the current workspace and index generation.",
                "details": {
                    "rejectedStaleCount": rejected,
                    "workspaceGeneration": self.gate.workspace_generation,
                    "indexGeneration": self.gate.index_generation,
                    "admissionThreshold": self.gate.admission_threshold(),
                }
            }));
        }
        codes
    }

    /// Render the canonical `ee.cache.hotset.v1` JSON artifact. Ordering is
    /// deterministic: search and pack entries are emitted sorted by
    /// `(kind, key)` (the same order [`HotsetManifestBuilder::build`] used
    /// to merge them). Volatile fields are caller-controlled (see
    /// `with_captured_at`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("schema".to_owned(), Value::String(SCHEMA.to_owned()));
        obj.insert(
            "workspaceId".to_owned(),
            Value::String(self.workspace_id.clone()),
        );
        obj.insert(
            "workspaceGeneration".to_owned(),
            json!(self.gate.workspace_generation),
        );
        obj.insert(
            "indexGeneration".to_owned(),
            json!(self.gate.index_generation),
        );
        obj.insert(
            "admissionThreshold".to_owned(),
            json!(self.gate.admission_threshold()),
        );
        if let Some(tier) = &self.profile_tier {
            obj.insert("profileTier".to_owned(), Value::String(tier.clone()));
        }
        if let Some(captured) = &self.captured_at {
            obj.insert("capturedAt".to_owned(), Value::String(captured.clone()));
        }
        obj.insert(
            "redactionStatus".to_owned(),
            Value::String(REDACTION_STATUS.to_owned()),
        );
        obj.insert("candidateCount".to_owned(), json!(self.candidate_count()));
        obj.insert("admittedCount".to_owned(), json!(self.admitted_count()));
        obj.insert(
            "rejectedStaleCount".to_owned(),
            json!(self.rejected_stale_count()),
        );
        obj.insert("memoryBudget".to_owned(), self.budget.to_json());
        obj.insert(
            "searchEntries".to_owned(),
            Value::Array(
                self.search_admitted
                    .iter()
                    .map(SearchHotsetEntry::data_json)
                    .collect(),
            ),
        );
        obj.insert(
            "packEntries".to_owned(),
            Value::Array(
                self.pack_admitted
                    .iter()
                    .map(PackHotsetEntry::data_json)
                    .collect(),
            ),
        );
        obj.insert(
            "rejectedStaleSearchEntries".to_owned(),
            Value::Array(
                self.search_rejected_stale
                    .iter()
                    .map(SearchHotsetEntry::data_json)
                    .collect(),
            ),
        );
        obj.insert(
            "rejectedStalePackEntries".to_owned(),
            Value::Array(
                self.pack_rejected_stale
                    .iter()
                    .map(PackHotsetEntry::data_json)
                    .collect(),
            ),
        );
        obj.insert("degraded".to_owned(), Value::Array(self.degraded_codes()));
        Value::Object(obj)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{PackHotsetEntry, PackHotsetEntryKind};
    use crate::search::SearchHotsetEntry;

    type TestResult = Result<(), String>;

    #[test]
    fn peer_human_attested_hotset_weight_sits_between_local_human_and_agent_validation() {
        assert!(
            trust_class_basis_points("human_explicit")
                > trust_class_basis_points("peer_human_attested")
        );
        assert!(
            trust_class_basis_points("peer_human_attested")
                > trust_class_basis_points("agent_validated")
        );
    }

    fn builder(threshold_gen: u64) -> HotsetManifestBuilder {
        HotsetManifestBuilder::new(
            "ws_01HQTEST0000000000000000",
            GenerationGate::new(threshold_gen, threshold_gen),
        )
        .with_profile_tier("balanced")
        .with_captured_at("2026-05-19T20:00:00Z")
        .with_budget(HotsetBudget::new(1024, 1_048_576).with_current(2, 256))
    }

    fn pack_selection_audit_entry(generation: u64, hit_count: u64) -> PackHotsetEntry {
        // Constructing the entry directly side-steps the `selection_audit`
        // factory, which would need a full `PackDraft`. The struct fields
        // are pub today, so this stays inside the contract surface.
        PackHotsetEntry {
            key: format!("pack:audit:{generation}"),
            kind: PackHotsetEntryKind::SelectionAudit,
            section: None,
            generation,
            estimated_bytes: 256,
            hit_count,
            redaction_status: "content_not_stored",
        }
    }

    #[test]
    fn happy_path_builds_manifest_with_admitted_entries() -> TestResult {
        let memory = SearchHotsetEntry::memory("mem_abc", 5, 3);
        let query = SearchHotsetEntry::query_shape("ee context release", 5, 2)
            .ok_or_else(|| "query shape should normalize".to_owned())?;
        let pack = pack_selection_audit_entry(5, 4);

        let manifest = builder(5)
            .search_entries([memory, query])
            .pack_entries([pack])
            .build();

        assert_eq!(manifest.candidate_count(), 3);
        assert_eq!(manifest.admitted_count(), 3);
        assert_eq!(manifest.rejected_stale_count(), 0);
        assert!(
            manifest.is_redaction_safe(),
            "all entries content_not_stored"
        );
        assert!(manifest.degraded_codes().is_empty());

        let json = manifest.to_json();
        assert_eq!(json["schema"], "ee.cache.hotset.v1");
        assert_eq!(json["redactionStatus"], "content_not_stored");
        assert_eq!(json["searchEntries"].as_array().map(Vec::len), Some(2));
        assert_eq!(json["packEntries"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            json["rejectedStaleSearchEntries"].as_array().map(Vec::len),
            Some(0)
        );
        Ok(())
    }

    #[test]
    fn stale_entries_are_rejected_and_emit_degraded_code() -> TestResult {
        let fresh_memory = SearchHotsetEntry::memory("mem_fresh", 10, 1);
        let stale_memory = SearchHotsetEntry::memory("mem_stale", 4, 1);
        let stale_pack = pack_selection_audit_entry(3, 1);

        let manifest = builder(10)
            .search_entries([fresh_memory, stale_memory])
            .pack_entries([stale_pack])
            .build();

        assert_eq!(manifest.candidate_count(), 3);
        assert_eq!(manifest.admitted_count(), 1);
        assert_eq!(manifest.rejected_stale_count(), 2);

        let codes = manifest.degraded_codes();
        assert_eq!(codes.len(), 1, "exactly one degraded code expected");
        let code = &codes[0];
        assert_eq!(code["code"], "cache_hotset_stale");
        assert_eq!(code["severity"], "medium");
        assert!(
            code["message"]
                .as_str()
                .unwrap_or_default()
                .contains("rejected 2 entries"),
            "message should report the rejected count, got {:?}",
            code["message"]
        );
        assert_eq!(code["details"]["rejectedStaleCount"], 2);
        assert_eq!(code["details"]["workspaceGeneration"], 10);
        Ok(())
    }

    #[test]
    fn empty_inputs_produce_zero_count_manifest_with_no_degraded_code() {
        let manifest = builder(7).build();
        assert_eq!(manifest.candidate_count(), 0);
        assert_eq!(manifest.admitted_count(), 0);
        assert_eq!(manifest.rejected_stale_count(), 0);
        assert!(manifest.degraded_codes().is_empty());
        assert!(manifest.is_redaction_safe());
        let json = manifest.to_json();
        assert_eq!(json["candidateCount"], 0);
        assert_eq!(json["searchEntries"], json!([]));
        assert_eq!(json["packEntries"], json!([]));
        assert_eq!(json["degraded"], json!([]));
    }

    #[test]
    fn duplicate_entries_merge_hit_counts_deterministically() -> TestResult {
        let memory_a = SearchHotsetEntry::memory("mem_dup", 5, 3);
        let memory_a_again = SearchHotsetEntry::memory("mem_dup", 5, 2);
        let memory_b = SearchHotsetEntry::memory("mem_other", 5, 1);

        let manifest = builder(5)
            .search_entries([memory_a, memory_a_again, memory_b])
            .build();

        assert_eq!(manifest.admitted_count(), 2, "duplicates merge");
        let json = manifest.to_json();
        let entries = json["searchEntries"]
            .as_array()
            .ok_or_else(|| "searchEntries should be array".to_owned())?;
        let dup_entry = entries
            .iter()
            .find(|entry| {
                entry["key"]
                    .as_str()
                    .is_some_and(|key| key.starts_with("blake3:") || key.contains("memory"))
                    && entry["hitCount"].as_u64() == Some(5)
            })
            .or_else(|| {
                entries
                    .iter()
                    .find(|entry| entry["hitCount"].as_u64() == Some(5))
            });
        assert!(
            dup_entry.is_some(),
            "merged entry should report hitCount=5 (3+2). entries={entries:?}"
        );
        Ok(())
    }

    #[test]
    fn stale_duplicate_search_entries_are_rejected_before_merge() {
        let fresh = SearchHotsetEntry::memory("mem_dup", 10, 2);
        let stale = SearchHotsetEntry::memory("mem_dup", 4, 9);

        let manifest = builder(10).search_entries([stale, fresh]).build();

        assert_eq!(manifest.admitted_count(), 1);
        assert_eq!(manifest.rejected_stale_count(), 1);
        let json = manifest.to_json();
        let admitted = json["searchEntries"].as_array().expect("search entries");
        let rejected = json["rejectedStaleSearchEntries"]
            .as_array()
            .expect("rejected search entries");
        assert_eq!(admitted.len(), 1);
        assert_eq!(rejected.len(), 1);
        assert_eq!(
            admitted[0]["hitCount"], 2,
            "fresh hit count should not absorb stale hits"
        );
        assert_eq!(rejected[0]["hitCount"], 9);
        assert_eq!(json["degraded"][0]["code"], "cache_hotset_stale");
    }

    #[test]
    fn stale_duplicate_pack_entries_are_rejected_before_merge() {
        let fresh = PackHotsetEntry {
            key: "pack:audit:duplicate".to_owned(),
            kind: PackHotsetEntryKind::SelectionAudit,
            section: None,
            generation: 10,
            estimated_bytes: 256,
            hit_count: 2,
            redaction_status: "content_not_stored",
        };
        let stale = PackHotsetEntry {
            key: "pack:audit:duplicate".to_owned(),
            kind: PackHotsetEntryKind::SelectionAudit,
            section: None,
            generation: 4,
            estimated_bytes: 512,
            hit_count: 9,
            redaction_status: "content_not_stored",
        };

        let manifest = builder(10).pack_entries([stale, fresh]).build();

        assert_eq!(manifest.admitted_count(), 1);
        assert_eq!(manifest.rejected_stale_count(), 1);
        let json = manifest.to_json();
        let admitted = json["packEntries"].as_array().expect("pack entries");
        let rejected = json["rejectedStalePackEntries"]
            .as_array()
            .expect("rejected pack entries");
        assert_eq!(admitted.len(), 1);
        assert_eq!(rejected.len(), 1);
        assert_eq!(
            admitted[0]["hitCount"], 2,
            "fresh hit count should not absorb stale hits"
        );
        assert_eq!(rejected[0]["hitCount"], 9);
        assert_eq!(json["degraded"][0]["details"]["rejectedStaleCount"], 1);
    }

    #[test]
    fn admission_threshold_uses_max_of_workspace_and_index_generation() {
        let gate = GenerationGate::new(7, 3);
        assert_eq!(gate.admission_threshold(), 7);

        let gate = GenerationGate::new(2, 8);
        assert_eq!(gate.admission_threshold(), 8);
    }

    #[test]
    fn json_output_is_byte_identical_across_runs_for_same_inputs() -> TestResult {
        let m1 = builder(5)
            .search_entries([
                SearchHotsetEntry::memory("mem_a", 5, 1),
                SearchHotsetEntry::memory("mem_b", 5, 2),
                SearchHotsetEntry::query_shape("ee context release", 5, 1)
                    .ok_or_else(|| "query shape should normalize".to_owned())?,
            ])
            .pack_entries([pack_selection_audit_entry(5, 3)])
            .build();
        let m2 = builder(5)
            // Different insertion order — output must still match.
            .pack_entries([pack_selection_audit_entry(5, 3)])
            .search_entries([
                SearchHotsetEntry::query_shape("ee context release", 5, 1)
                    .ok_or_else(|| "query shape should normalize".to_owned())?,
                SearchHotsetEntry::memory("mem_b", 5, 2),
                SearchHotsetEntry::memory("mem_a", 5, 1),
            ])
            .build();

        let s1 = serde_json::to_string(&m1.to_json()).map_err(|e| e.to_string())?;
        let s2 = serde_json::to_string(&m2.to_json()).map_err(|e| e.to_string())?;
        assert_eq!(s1, s2, "manifest JSON must be byte-identical");
        Ok(())
    }

    #[test]
    fn manifest_never_contains_raw_query_text_or_memory_content() -> TestResult {
        let secret = "DATABASE_URL=postgres://user:hunter2@host/db";
        let secret_id = "mem_secret_marker";

        let entry = SearchHotsetEntry::query_shape(secret, 5, 1)
            .ok_or_else(|| "query shape should normalize".to_owned())?;
        let memory = SearchHotsetEntry::memory(secret_id, 5, 1);

        let manifest = builder(5).search_entries([entry, memory]).build();
        let json = manifest.to_json();
        let serialized = serde_json::to_string(&json).map_err(|e| e.to_string())?;

        assert!(
            !serialized.contains("hunter2"),
            "raw secret value must not leak into hotset JSON"
        );
        assert!(
            !serialized.contains("DATABASE_URL"),
            "raw query text must not leak into hotset JSON"
        );
        // memory IDs ARE included intentionally (the bead spec says
        // `memory_id` references are stored, content is not); guard the
        // intent so a future refactor doesn't accidentally remove them.
        assert!(
            serialized.contains(secret_id) || !serialized.contains(&format!("\"{secret_id}\"")),
            "memory id may appear as redaction-safe reference"
        );
        assert!(manifest.is_redaction_safe());
        Ok(())
    }

    #[test]
    fn rejected_stale_entries_keep_redaction_invariant() {
        let stale = SearchHotsetEntry::memory("mem_stale", 1, 1);
        let manifest = builder(10).search_entries([stale]).build();
        assert_eq!(manifest.rejected_stale_count(), 1);
        assert!(
            manifest.is_redaction_safe(),
            "rejected entries must still be redaction-safe"
        );
        let json = manifest.to_json();
        let rejected = json["rejectedStaleSearchEntries"]
            .as_array()
            .expect("array");
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0]["redactionStatus"], "content_not_stored");
    }

    #[test]
    fn memory_budget_round_trips_through_json() {
        let manifest = HotsetManifestBuilder::new("ws_budget", GenerationGate::new(1, 1))
            .with_budget(HotsetBudget::new(2048, 8 * 1024).with_current(7, 512))
            .build();

        let json = manifest.to_json();
        assert_eq!(json["memoryBudget"]["maxEntries"], 2048);
        assert_eq!(json["memoryBudget"]["maxBytes"], 8 * 1024);
        assert_eq!(json["memoryBudget"]["currentEntries"], 7);
        assert_eq!(json["memoryBudget"]["currentBytes"], 512);
    }

    fn tier_input(memory_id: &str, score: f64) -> MemoryTierInput {
        MemoryTierInput::from_normalized_scores(memory_id, "ws-tier", score, score, score, score)
            .with_access(20, 10)
            .with_trust_class("agent_validated")
    }

    #[test]
    fn memory_tier_policy_is_deterministic_for_same_inputs() -> TestResult {
        let config = MemoryTierPolicyConfig::new(1, 2, 700);
        let inputs = [
            tier_input("mem_b", 0.8),
            tier_input("mem_a", 0.8),
            tier_input("mem_c", 0.4),
        ];
        let reversed = [
            tier_input("mem_c", 0.4),
            tier_input("mem_a", 0.8),
            tier_input("mem_b", 0.8),
        ];

        let first = memory_storage_tier_policy_json(inputs, config);
        let second = memory_storage_tier_policy_json(reversed, config);
        let s1 = serde_json::to_string(&first).map_err(|err| err.to_string())?;
        let s2 = serde_json::to_string(&second).map_err(|err| err.to_string())?;

        assert_eq!(s1, s2, "tier policy output must be deterministic");
        assert_eq!(first["schema"], MEMORY_TIER_POLICY_SCHEMA);
        assert_eq!(first["assignments"][0]["memoryId"], "mem_a");
        assert_eq!(first["assignments"][0]["tier"], "hot");
        Ok(())
    }

    #[test]
    fn memory_tier_policy_respects_hot_floor_and_warm_budget() {
        let below_floor = assign_memory_storage_tiers(
            [tier_input("mem_high", 0.95), tier_input("mem_mid", 0.60)],
            MemoryTierPolicyConfig::new(2, 1, 850),
        );

        assert_eq!(below_floor[0].tier, MemoryStorageTier::Hot);
        assert_eq!(
            below_floor[1].tier,
            MemoryStorageTier::Warm,
            "below-floor candidate should not become hot even inside hot budget"
        );

        let capped = assign_memory_storage_tiers(
            [
                tier_input("mem_high", 0.95),
                tier_input("mem_mid", 0.60),
                tier_input("mem_low", 0.10),
            ],
            MemoryTierPolicyConfig::new(1, 1, 700),
        );
        assert_eq!(capped[0].tier, MemoryStorageTier::Hot);
        assert_eq!(capped[1].tier, MemoryStorageTier::Warm);
        assert_eq!(capped[2].tier, MemoryStorageTier::Cold);
    }

    #[test]
    fn memory_tier_policy_marks_required_evidence_preserved_even_when_cold() {
        let required = MemoryTierInput::from_normalized_scores(
            "mem_required_failure",
            "ws-tier",
            0.05,
            0.05,
            0.05,
            0.05,
        )
        .with_safety_or_failure_evidence(true);
        let assignments = assign_memory_storage_tiers(
            [tier_input("mem_hot", 0.95), required],
            MemoryTierPolicyConfig::new(1, 0, 700),
        );

        let required = assignments
            .iter()
            .find(|assignment| assignment.memory_id == "mem_required_failure")
            .expect("required evidence assignment");
        assert_eq!(required.tier, MemoryStorageTier::Cold);
        assert!(required.required_evidence_preserved);
        assert_eq!(
            required.tier_assignment_reason,
            "cold_required_evidence_preserved"
        );
    }

    #[test]
    fn memory_tier_policy_keeps_low_score_required_evidence_cold_inside_warm_budget() {
        let required = MemoryTierInput::from_normalized_scores(
            "mem_required_low_score",
            "ws-tier",
            0.05,
            0.05,
            0.05,
            0.05,
        )
        .with_explicit_query_match(true)
        .with_safety_or_failure_evidence(true);
        let assignments = assign_memory_storage_tiers(
            [
                tier_input("mem_hot", 0.95),
                tier_input("mem_warm", 0.60),
                required,
            ],
            MemoryTierPolicyConfig::new(1, 8, 700),
        );

        let required = assignments
            .iter()
            .find(|assignment| assignment.memory_id == "mem_required_low_score")
            .expect("required evidence assignment");
        assert_eq!(required.tier, MemoryStorageTier::Cold);
        assert!(required.required_evidence_preserved);
        assert_eq!(
            required.tier_assignment_reason,
            "cold_required_evidence_preserved"
        );
    }

    #[test]
    fn memory_tier_policy_quantizes_invalid_and_out_of_range_scores() {
        let input = MemoryTierInput::from_normalized_scores(
            "mem_quantized",
            "ws-tier",
            f64::NAN,
            -1.0,
            2.0,
            f64::INFINITY,
        )
        .with_access(u64::MAX, u64::MAX)
        .with_trust_class("human_explicit");
        let assignments =
            assign_memory_storage_tiers([input], MemoryTierPolicyConfig::new(1, 0, 0));

        assert_eq!(assignments.len(), 1);
        assert!(
            assignments[0].tier_score <= 1000,
            "score must stay in basis-point range"
        );
        assert_eq!(assignments[0].policy_version, MEMORY_TIER_POLICY_VERSION);
    }

    fn tier_assignment(
        memory_id: &str,
        tier: MemoryStorageTier,
        score: u16,
    ) -> MemoryTierAssignment {
        MemoryTierAssignment {
            memory_id: memory_id.to_owned(),
            workspace_id: "ws-tier".to_owned(),
            tier,
            tier_score: score,
            tier_assignment_reason: "test_assignment",
            deterministic_tie_break_key: format!("ws-tier:{memory_id}"),
            policy_version: MEMORY_TIER_POLICY_VERSION,
            required_evidence_preserved: false,
        }
    }

    fn previous_tier(
        memory_id: &str,
        tier: MemoryStorageTier,
        score: u16,
    ) -> MemoryTierPreviousState {
        MemoryTierPreviousState::new(
            memory_id,
            "ws-tier",
            tier,
            score,
            MEMORY_TIER_POLICY_VERSION,
        )
    }

    #[test]
    fn memory_tier_transition_plan_classifies_metadata_only_changes() {
        let options = MemoryTierTransitionOptions::new("2026-05-22T23:30:00Z");
        let demotion_counters = MemoryTierTransitionCounters::new(2, 0, 250, 300, 450);
        let plan = plan_memory_tier_transitions(
            [
                MemoryTierTransitionInput::new(tier_assignment(
                    "mem_new",
                    MemoryStorageTier::Hot,
                    930,
                )),
                MemoryTierTransitionInput::new(tier_assignment(
                    "mem_promote",
                    MemoryStorageTier::Hot,
                    880,
                ))
                .with_previous(previous_tier(
                    "mem_promote",
                    MemoryStorageTier::Warm,
                    650,
                )),
                MemoryTierTransitionInput::new(tier_assignment(
                    "mem_demote",
                    MemoryStorageTier::Warm,
                    590,
                ))
                .with_previous(previous_tier("mem_demote", MemoryStorageTier::Hot, 860))
                .with_counters(demotion_counters),
                MemoryTierTransitionInput::new(tier_assignment(
                    "mem_evict",
                    MemoryStorageTier::Cold,
                    120,
                ))
                .with_previous(previous_tier(
                    "mem_evict",
                    MemoryStorageTier::Hot,
                    790,
                )),
                MemoryTierTransitionInput::new(tier_assignment(
                    "mem_retain",
                    MemoryStorageTier::Warm,
                    620,
                ))
                .with_previous(previous_tier(
                    "mem_retain",
                    MemoryStorageTier::Warm,
                    615,
                )),
            ],
            options,
        );

        assert_eq!(plan.schema(), MEMORY_TIER_TRANSITION_AUDIT_SCHEMA);
        assert_eq!(plan.input_count(), 5);
        assert_eq!(plan.transition_count(MemoryTierTransitionKind::Admit), 1);
        assert_eq!(plan.transition_count(MemoryTierTransitionKind::Promote), 1);
        assert_eq!(plan.transition_count(MemoryTierTransitionKind::Demote), 1);
        assert_eq!(plan.transition_count(MemoryTierTransitionKind::Evict), 1);
        assert_eq!(plan.transition_count(MemoryTierTransitionKind::Retain), 1);

        let json = plan.to_json();
        assert_eq!(json["schema"], MEMORY_TIER_TRANSITION_AUDIT_SCHEMA);
        assert_eq!(json["metadataOnly"], true);
        assert_eq!(json["transitionCounts"]["evict"], 1);

        let audits = json["audits"].as_array().expect("audit array");
        let demote = audits
            .iter()
            .find(|audit| audit["memoryId"] == "mem_demote")
            .expect("demote audit");
        assert_eq!(demote["transition"], "demote");
        assert_eq!(demote["reason"], "demote_decay_or_trust_penalty");
        assert_eq!(
            demote["sourceCounters"]["decayPenaltyBasisPoints"],
            demotion_counters.decay_penalty_basis_points
        );

        let evict = audits
            .iter()
            .find(|audit| audit["memoryId"] == "mem_evict")
            .expect("evict audit");
        assert_eq!(evict["previousTier"], "hot");
        assert_eq!(evict["newTier"], "cold");
        assert_eq!(evict["reason"], "evict_to_cold_metadata_only");
        assert_eq!(evict["metadataOnly"], true);
    }

    #[test]
    fn memory_tier_transition_plan_is_deterministic_and_bounded() -> TestResult {
        let options =
            MemoryTierTransitionOptions::new("2026-05-22T23:31:00Z").with_max_transitions(2);
        let first = plan_memory_tier_transitions(
            [
                MemoryTierTransitionInput::new(tier_assignment(
                    "mem_c",
                    MemoryStorageTier::Cold,
                    100,
                ))
                .with_previous(previous_tier(
                    "mem_c",
                    MemoryStorageTier::Warm,
                    600,
                )),
                MemoryTierTransitionInput::new(tier_assignment(
                    "mem_a",
                    MemoryStorageTier::Hot,
                    900,
                )),
                MemoryTierTransitionInput::new(tier_assignment(
                    "mem_b",
                    MemoryStorageTier::Warm,
                    650,
                ))
                .with_previous(previous_tier(
                    "mem_b",
                    MemoryStorageTier::Cold,
                    250,
                )),
            ],
            options.clone(),
        );
        let second = plan_memory_tier_transitions(
            [
                MemoryTierTransitionInput::new(tier_assignment(
                    "mem_b",
                    MemoryStorageTier::Warm,
                    650,
                ))
                .with_previous(previous_tier(
                    "mem_b",
                    MemoryStorageTier::Cold,
                    250,
                )),
                MemoryTierTransitionInput::new(tier_assignment(
                    "mem_c",
                    MemoryStorageTier::Cold,
                    100,
                ))
                .with_previous(previous_tier(
                    "mem_c",
                    MemoryStorageTier::Warm,
                    600,
                )),
                MemoryTierTransitionInput::new(tier_assignment(
                    "mem_a",
                    MemoryStorageTier::Hot,
                    900,
                )),
            ],
            options,
        );

        let s1 = serde_json::to_string(&first.to_json()).map_err(|err| err.to_string())?;
        let s2 = serde_json::to_string(&second.to_json()).map_err(|err| err.to_string())?;
        assert_eq!(s1, s2, "bounded transition batches must be stable");
        assert_eq!(first.input_count(), 3);
        assert_eq!(first.audits().len(), 2);
        assert_eq!(first.audits()[0].memory_id, "mem_a");
        assert_eq!(first.audits()[1].memory_id, "mem_b");
        Ok(())
    }

    #[test]
    fn memory_tier_transition_dry_run_and_write_plan_share_audit_identity() {
        let input = MemoryTierTransitionInput::new(tier_assignment(
            "mem_shared",
            MemoryStorageTier::Warm,
            610,
        ))
        .with_previous(previous_tier("mem_shared", MemoryStorageTier::Cold, 120));
        let dry_run = plan_memory_tier_transitions(
            [input.clone()],
            MemoryTierTransitionOptions::new("2026-05-22T23:32:00Z").with_dry_run(true),
        );
        let write_plan = plan_memory_tier_transitions(
            [input],
            MemoryTierTransitionOptions::new("2026-05-22T23:32:00Z").with_dry_run(false),
        );

        let dry_audit = &dry_run.audits()[0];
        let write_audit = &write_plan.audits()[0];
        assert_eq!(dry_audit.memory_id, write_audit.memory_id);
        assert_eq!(dry_audit.transition, write_audit.transition);
        assert_eq!(
            dry_audit.deterministic_tie_break_key,
            write_audit.deterministic_tie_break_key
        );
        assert!(dry_audit.dry_run);
        assert!(!write_audit.dry_run);
    }

    fn bead_signal(id: &str, summary: &str) -> PrewarmSignal {
        PrewarmSignal::new(PrewarmSignalSource::Beads, id, summary)
            .with_labels(["context", "prewarm", "swarm-scale"])
            .with_priority(2)
    }

    #[test]
    fn prewarm_plan_is_deterministic_for_same_signals() -> TestResult {
        let bead = bead_signal(
            "bd-1zb7k.17.3",
            "Context hotset prewarm from Beads BV and Agent Mail signals",
        );
        let mail = PrewarmSignal::new(
            PrewarmSignalSource::AgentMail,
            "thread-hotset",
            "Context hotset prewarm from Beads BV and Agent Mail signals",
        )
        .with_labels(["context", "prewarm", "swarm-scale"])
        .with_priority(2);

        let budget = HotsetBudget::new(16, 16 * 1024);
        let p1 = plan_context_hotset_prewarm([bead.clone(), mail.clone()], 42, budget, 8);
        let p2 = plan_context_hotset_prewarm([mail, bead], 42, budget, 8);

        let s1 = serde_json::to_string(&p1.to_json()).map_err(|err| err.to_string())?;
        let s2 = serde_json::to_string(&p2.to_json()).map_err(|err| err.to_string())?;
        assert_eq!(s1, s2, "prewarm plan JSON must be deterministic");
        assert_eq!(p1.schema(), "ee.cache.hotset_prewarm_plan.v1");
        assert_eq!(p1.input_signal_count(), 2);
        Ok(())
    }

    #[test]
    fn prewarm_plan_merges_duplicate_query_shapes_across_sources() -> TestResult {
        let summary = "Shard fanout global timeline audit chain";
        let bead = PrewarmSignal::new(PrewarmSignalSource::Beads, "bd-f6jfs.6", summary)
            .with_labels(["audit", "shard"])
            .with_priority(1);
        let bv = PrewarmSignal::new(PrewarmSignalSource::Bv, "bv-bottleneck-1", summary)
            .with_labels(["audit", "shard"])
            .with_priority(1);

        let plan = plan_context_hotset_prewarm([bead, bv], 7, HotsetBudget::new(8, 8 * 1024), 8);

        assert_eq!(plan.candidates().len(), 1);
        let json = plan.to_json();
        let candidate = &json["candidates"][0];
        assert_eq!(candidate["sourceKinds"], json!(["beads", "bv"]));
        assert_eq!(
            candidate["signalRefHashes"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(json["searchEntries"].as_array().map(Vec::len), Some(1));
        assert_eq!(json["cachePosture"]["status"], "admissible");
        Ok(())
    }

    #[test]
    fn prewarm_plan_caps_candidates_by_score_then_hash() {
        let high = bead_signal("bd-high", "context pack prewarm hot path").with_priority(1);
        let low = PrewarmSignal::new(
            PrewarmSignalSource::HostProfile,
            "host-cold",
            "host profile low memory pressure",
        )
        .with_priority(8);

        let uncapped = plan_context_hotset_prewarm(
            [high.clone(), low.clone()],
            9,
            HotsetBudget::new(8, 8 * 1024),
            0,
        );
        assert_eq!(uncapped.candidates().len(), 2);

        let capped = plan_context_hotset_prewarm([high, low], 9, HotsetBudget::new(8, 8 * 1024), 1);
        assert_eq!(capped.candidates().len(), 1);
        assert!(
            capped.candidates()[0].score() >= uncapped.candidates()[1].score(),
            "highest-score candidate should survive cap"
        );
    }

    #[test]
    fn prewarm_plan_does_not_emit_raw_signal_text() -> TestResult {
        let secret = "DATABASE_URL=postgres://user:hunter2@host/db";
        let mail = PrewarmSignal::new(PrewarmSignalSource::AgentMail, "thread-secret", secret)
            .with_labels(["credential:do-not-leak", "context"])
            .with_priority(1);

        let plan = plan_context_hotset_prewarm([mail], 3, HotsetBudget::new(8, 8 * 1024), 8);
        let serialized = serde_json::to_string(&plan.to_json()).map_err(|err| err.to_string())?;

        assert!(!serialized.contains("hunter2"));
        assert!(!serialized.contains("DATABASE_URL"));
        assert!(!serialized.contains("credential:do-not-leak"));
        assert!(serialized.contains("query_hashes_only"));
        Ok(())
    }

    #[test]
    fn prewarm_plan_empty_inputs_surface_degraded_code() {
        let plan = plan_context_hotset_prewarm([], 1, HotsetBudget::new(8, 8 * 1024), 8);
        assert!(plan.candidates().is_empty());
        assert_eq!(plan.skipped_signal_count(), 0);

        let json = plan.to_json();
        assert_eq!(json["candidateCount"], 0);
        assert_eq!(json["degraded"][0]["code"], "hotset_prewarm_no_signals");
    }

    #[test]
    fn cache_prewarm_reports_search_admission_evidence_without_fake_latency() -> TestResult {
        let manifest = builder(10)
            .search_entries([
                SearchHotsetEntry::memory("mem-search-a", 10, 3),
                SearchHotsetEntry::memory("mem-search-b", 10, 2),
            ])
            .build()
            .to_json();

        let report = cache_prewarm_report_from_manifest_json(
            &manifest,
            &CachePrewarmOptions::new("balanced", CacheBudget::new(16, 16 * 1024))
                .with_current_generation(Some(10)),
        )
        .map_err(|error| error.to_string())?;

        let search_report = &report["reports"]["search"];
        assert_eq!(
            search_report["prewarmEvidence"]["evidenceKind"],
            "search_hotset_admission"
        );
        assert_eq!(search_report["prewarmEvidence"]["requestedHitCount"], 5);
        assert!(search_report.get("benchmarkEvidence").is_none());
        let latency = &report["latencyEstimate"];
        assert!(
            latency["unmeasuredComponents"]
                .as_array()
                .is_some_and(|components| components.iter().any(|component| {
                    component["component"] == "search"
                        && component["reason"]
                            == "search_prewarm_reports_admission_stats_not_latency"
                })),
            "search must be marked unmeasured instead of folded in as zero latency: {latency:?}"
        );
        Ok(())
    }

    #[test]
    fn cache_prewarm_allow_stale_admits_mixed_generation_hotset() -> TestResult {
        let manifest = builder(5)
            .search_entries([
                SearchHotsetEntry::memory("mem-search-old", 5, 3),
                SearchHotsetEntry::memory("mem-search-newer", 6, 2),
            ])
            .pack_entries([
                pack_selection_audit_entry(5, 4),
                pack_selection_audit_entry(6, 1),
            ])
            .build()
            .to_json();

        let report = cache_prewarm_report_from_manifest_json(
            &manifest,
            &CachePrewarmOptions::new("balanced", CacheBudget::new(16, 16 * 1024))
                .with_current_generation(Some(8))
                .with_allow_stale_hotset(true),
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(report["allowStaleHotset"], true);
        assert_eq!(report["admitted"]["searchEntries"], 2);
        assert_eq!(report["admitted"]["packEntries"], 2);
        assert_eq!(report["rejected"]["totalEntries"], 0);
        assert_eq!(report["reports"]["search"]["status"], "warm");
        assert_eq!(report["reports"]["pack"]["status"], "warm");
        assert_eq!(report["reports"]["search"]["currentGeneration"], 8);
        assert_eq!(report["reports"]["pack"]["currentGeneration"], 8);
        assert!(
            report["degraded"].as_array().is_some_and(|codes| {
                codes.iter().any(|code| {
                    code["code"] == STALE_HOTSET_CODE && code["details"]["allowStaleHotset"] == true
                })
            }),
            "stale admission should remain visible in degraded[]: {report:?}"
        );
        Ok(())
    }

    #[test]
    fn tier_aware_prewarm_counts_tiers_without_hiding_required_cold() -> TestResult {
        let manifest = builder(11)
            .search_entries([
                SearchHotsetEntry::memory("mem_hot", 11, 5),
                SearchHotsetEntry::memory("mem_warm", 11, 4),
                SearchHotsetEntry::memory("mem_cold", 11, 3),
            ])
            .build()
            .to_json();
        let required_cold = MemoryTierInput::from_normalized_scores(
            "mem_required_cold",
            "ws-tier",
            0.05,
            0.05,
            0.05,
            0.05,
        )
        .with_mandatory_provenance(true);
        let assignments = assign_memory_storage_tiers(
            [
                tier_input("mem_hot", 0.95),
                tier_input("mem_warm", 0.60),
                tier_input("mem_cold", 0.20),
                required_cold,
            ],
            MemoryTierPolicyConfig::new(1, 1, 700),
        );

        let report = tier_aware_cache_prewarm_report_from_manifest_json(
            &manifest,
            assignments,
            11,
            &CachePrewarmOptions::new("balanced", CacheBudget::new(16, 16 * 1024))
                .with_current_generation(Some(11)),
        )
        .map_err(|error| error.to_string())?;

        let posture = &report["memoryTierPosture"];
        assert_eq!(posture["status"], "fresh");
        assert_eq!(posture["admittedHotCount"], 1);
        assert_eq!(posture["admittedWarmCount"], 1);
        assert_eq!(posture["admittedColdCount"], 1);
        assert_eq!(posture["coldRecallSkippedCount"], 1);
        assert_eq!(posture["requiredColdEvidenceCount"], 1);
        assert_eq!(posture["preservesColdRecallEligibility"], true);
        assert!(report["degraded"].as_array().is_none_or(|codes| {
            codes
                .iter()
                .all(|code| code["code"] != MEMORY_TIER_METADATA_STALE_CODE)
        }));
        Ok(())
    }

    #[test]
    fn tier_aware_prewarm_rejects_stale_tier_metadata() -> TestResult {
        let manifest = builder(12)
            .search_entries([SearchHotsetEntry::memory("mem_hot", 12, 5)])
            .build()
            .to_json();
        let assignments = assign_memory_storage_tiers(
            [tier_input("mem_hot", 0.95), tier_input("mem_warm", 0.60)],
            MemoryTierPolicyConfig::new(1, 1, 700),
        );

        let report = tier_aware_cache_prewarm_report_from_manifest_json(
            &manifest,
            assignments,
            9,
            &CachePrewarmOptions::new("balanced", CacheBudget::new(16, 16 * 1024))
                .with_current_generation(Some(12)),
        )
        .map_err(|error| error.to_string())?;

        let posture = &report["memoryTierPosture"];
        assert_eq!(posture["status"], "stale_rejected");
        assert_eq!(posture["tierGeneration"], 9);
        assert_eq!(posture["currentGeneration"], 12);
        assert_eq!(posture["admittedHotCount"], 0);
        assert_eq!(posture["staleTierRejectedCount"], 2);
        let degraded = report["degraded"]
            .as_array()
            .ok_or_else(|| "degraded should be an array".to_owned())?;
        assert!(
            degraded
                .iter()
                .any(|code| code["code"] == MEMORY_TIER_METADATA_STALE_CODE),
            "stale tier metadata must emit {MEMORY_TIER_METADATA_STALE_CODE}: {degraded:?}"
        );
        Ok(())
    }

    #[test]
    fn tier_aware_prewarm_is_deterministic_for_assignment_order() -> TestResult {
        let manifest = builder(13)
            .search_entries([
                SearchHotsetEntry::memory("mem_a", 13, 5),
                SearchHotsetEntry::memory("mem_b", 13, 4),
            ])
            .build()
            .to_json();
        let assignments = assign_memory_storage_tiers(
            [tier_input("mem_b", 0.8), tier_input("mem_a", 0.8)],
            MemoryTierPolicyConfig::new(1, 1, 700),
        );
        let reversed = assignments.iter().rev().cloned().collect::<Vec<_>>();
        let options = CachePrewarmOptions::new("balanced", CacheBudget::new(16, 16 * 1024))
            .with_current_generation(Some(13));

        let first = tier_aware_cache_prewarm_report_from_manifest_json(
            &manifest,
            assignments,
            13,
            &options,
        )
        .map_err(|error| error.to_string())?;
        let second =
            tier_aware_cache_prewarm_report_from_manifest_json(&manifest, reversed, 13, &options)
                .map_err(|error| error.to_string())?;

        let first_json = serde_json::to_string(&first).map_err(|error| error.to_string())?;
        let second_json = serde_json::to_string(&second).map_err(|error| error.to_string())?;
        assert_eq!(first_json, second_json);
        Ok(())
    }
}
