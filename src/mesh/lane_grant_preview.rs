//! SRR6.46.17 — Pre-grant lane visibility audit (pure decision module).
//!
//! Before a user runs `ee mesh grant <peer> --lane <lane>` to widen the
//! conservative auto-enrollment defaults (body / embedding / graph_link
//! deny by default), they need to see EXACTLY which memories would
//! become visible. Without that surface, working out exposure requires
//! reasoning across SRR6.5 trust, SRR6.30 scope, redaction classes, and
//! memory tags — practically impossible.
//!
//! This module owns the **pure visibility computation** and its schema
//! constants. It does not touch the database, does not consult the
//! Tailscale CLI, does not emit audit rows: the caller hands in a
//! resolved slice of [`MemoryView`] (already redaction-aware), the
//! current and proposed [`IntendedLanePolicy`], the target [`Lane`]
//! being granted, and a small set of caller-side facts (peer-in-group,
//! sample strategy). The output is a fully populated
//! [`LaneGrantPreview`] envelope shaped to the documented
//! `ee.mesh.lane_grant_preview.v1` schema, ready for any renderer.
//!
//! Why a separate module rather than folding this into the auto-enroll
//! flow: (1) the preview is a pure read with a strict "no DB writes,
//! no audit rows" invariant, while auto-enroll is mutating; (2) the
//! visibility math is non-trivial and earns its own focused test
//! surface; (3) keeping the schema constants here lets the CLI / MCP
//! renderer follow-up wire without re-deriving them.
//!
//! The CLI / MCP / renderer surface (`ee mesh preview-grant`,
//! `ee_mesh_preview_grant` MCP tool) lands in a follow-up slice to
//! avoid touching `src/cli/mod.rs` while other agents hold
//! reservations there.

use std::cmp::Reverse;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::mesh::auto_enrollment_safety::{IntendedLanePolicy, LaneDecision};

/// JSON schema identifier for the lane-grant preview output. Held as the
/// source-of-truth constant so the renderer and the schema-lifecycle
/// drift gate agree.
pub const LANE_GRANT_PREVIEW_SCHEMA_V1: &str = "ee.mesh.lane_grant_preview.v1";

/// Degraded code emitted (informational) when the lane-grant preview
/// runs against a peer that is not in the workspace's auto-enrolled
/// peer-group. The preview still produces a valid envelope — the
/// caution surfaces the operator misunderstanding without aborting.
pub const LANE_GRANT_PREVIEW_PEER_NOT_IN_GROUP_CODE: &str = "lane_grant_preview_peer_not_in_group";

/// Degraded code emitted (info) when the proposed lane is already
/// granted in the current policy. Preview is still useful (it shows
/// what's currently exposed) but the user almost certainly didn't mean
/// to "grant" something already allowed.
pub const LANE_GRANT_PREVIEW_LANE_ALREADY_GRANTED_CODE: &str =
    "lane_grant_preview_lane_already_granted";

/// Default number of preview rows when the caller omits `--limit`.
pub const LANE_GRANT_PREVIEW_DEFAULT_LIMIT: usize = 25;

/// Hard ceiling on preview rows even when the caller passes `--limit`.
/// Prevents huge workspaces from producing 500MB+ preview envelopes.
pub const LANE_GRANT_PREVIEW_MAX_LIMIT: usize = 500;

/// Number of characters of memory body to include per preview row.
/// The body content is assumed pre-redacted by the caller; this module
/// only truncates and counts grapheme bytes as a defensive measure.
pub const LANE_GRANT_PREVIEW_CONTENT_PREVIEW_CHARS: usize = 100;

/// Threshold above which `large_volume_exposure` caution fires.
pub const LANE_GRANT_PREVIEW_LARGE_VOLUME_THRESHOLD: u64 = 1000;

/// Canonical sensitive-tag vocabulary that triggers
/// `sensitive_tags_in_exposure`. Kept here so the rule is self-evident
/// from the module surface; callers cannot extend the list (extending
/// it must land here so the schema documentation stays accurate).
pub const SENSITIVE_TAGS: &[&str] = &["secret", "private", "personal", "internal"];

// ============================================================================
// Lane and SampleStrategy enums
// ============================================================================

/// The six trust-lane channels a peer-group binding can grant or deny.
/// Matches the field names on [`IntendedLanePolicy`] one-for-one so
/// schema serialization and decision lookup share the same canonical
/// string set.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    Metadata,
    Body,
    Embedding,
    GraphLink,
    CurationSignal,
    RevisionNotice,
}

impl Lane {
    /// Canonical wire string used in `ee.mesh.lane_grant_preview.v1`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Body => "body",
            Self::Embedding => "embedding",
            Self::GraphLink => "graph_link",
            Self::CurationSignal => "curation_signal",
            Self::RevisionNotice => "revision_notice",
        }
    }

    /// Extract this lane's [`LaneDecision`] from an [`IntendedLanePolicy`].
    /// The two structs are kept in lock-step so this is a total function.
    #[must_use]
    pub fn decision_in(self, policy: &IntendedLanePolicy) -> LaneDecision {
        match self {
            Self::Metadata => policy.metadata,
            Self::Body => policy.body,
            Self::Embedding => policy.embedding,
            Self::GraphLink => policy.graph_link,
            Self::CurationSignal => policy.curation_signal,
            Self::RevisionNotice => policy.revision_notice,
        }
    }
}

/// Strategy for choosing which memories appear in the preview sample
/// when the total exposed count exceeds the requested limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleStrategy {
    /// Representative sample: deterministic shuffle by hashing
    /// `(memory_id, random_seed)` so identical inputs reproduce identical
    /// previews. "Random" is a misnomer — it is `deterministic-random`
    /// for the same seed. Pinned for test reproducibility.
    Random,
    /// Sort by trust class with [`TrustClass::HumanExplicit`] first.
    /// Useful for "what high-authority memories would leak?" audits.
    HighestTrust,
    /// Sort by `created_at_secs` descending (newest first). Useful for
    /// "what would the peer see if I granted this right now?" audits.
    MostRecent,
}

impl SampleStrategy {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::HighestTrust => "highest-trust",
            Self::MostRecent => "most-recent",
        }
    }
}

// ============================================================================
// TrustClass — SRR6.5 trust authority class
// ============================================================================

/// SRR6.5 trust authority class. Higher classes are more trustworthy
/// (and therefore more sensitive to leak). Order matters: the
/// `Ord`/`PartialOrd` derive uses declaration order, so
/// `HumanExplicit < HumanRevised < AgentValidated < AgentProposed <
/// External` — but the documented semantic is that **earlier variants
/// have higher trust** for the purposes of the
/// `high_trust_class_exposure` caution. Callers consult [`TrustClass::is_high_trust`]
/// rather than relying on derived ordering to avoid the inversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    HumanExplicit,
    HumanRevised,
    AgentValidated,
    AgentProposed,
    External,
}

impl TrustClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HumanExplicit => "human_explicit",
            Self::HumanRevised => "human_revised",
            Self::AgentValidated => "agent_validated",
            Self::AgentProposed => "agent_proposed",
            Self::External => "external",
        }
    }

    /// Score for sort ordering under [`SampleStrategy::HighestTrust`].
    /// Higher score = more trusted (and therefore picked first).
    #[must_use]
    pub fn trust_score(self) -> u8 {
        match self {
            Self::HumanExplicit => 5,
            Self::HumanRevised => 4,
            Self::AgentValidated => 3,
            Self::AgentProposed => 2,
            Self::External => 1,
        }
    }

    /// Whether this trust class triggers the `high_trust_class_exposure`
    /// caution when the memory would be exposed by the proposed grant.
    /// Only `HumanExplicit` qualifies: those are the user's directly
    /// authored rules, the most surprising thing to leak.
    #[must_use]
    pub fn is_high_trust(self) -> bool {
        matches!(self, Self::HumanExplicit)
    }
}

// ============================================================================
// Inputs
// ============================================================================

/// Per-memory facts the caller hands in. Borrowed to keep the preview
/// path allocation-light over very large memory slices.
#[derive(Clone, Copy, Debug)]
pub struct MemoryView<'a> {
    pub memory_id: &'a str,
    pub level: &'a str,
    pub kind: &'a str,
    /// Caller is responsible for any content redaction (secret-detector
    /// pass, tailscale_metadata strip, etc) before passing the body in.
    /// This module only truncates to [`LANE_GRANT_PREVIEW_CONTENT_PREVIEW_CHARS`].
    pub content: &'a str,
    pub tags: &'a [String],
    pub trust_class: TrustClass,
    /// Names of fields the redaction pipeline already stripped from
    /// this memory before it reached us. Reported into the preview row
    /// so the operator sees exactly which fields are hidden.
    pub redacted_fields: &'a [String],
    pub created_at_secs: i64,
    pub is_tombstoned: bool,
    /// Whether the memory would still be hidden via a redaction-class
    /// rule even after the lane is granted (e.g. an `api_key`-tagged
    /// memory body never crosses the body lane). When `true`, the
    /// memory is counted in `redactedFromExposureCount` and its
    /// preview row's `wouldExposeUnderProposedPolicy` is `false`.
    pub blocked_by_redaction_class: bool,
}

/// All inputs to [`compute_lane_grant_preview`]. Pure-data; no DB
/// handle, no `&Cx`, no I/O.
#[derive(Clone, Copy, Debug)]
pub struct LaneGrantPreviewInput<'a> {
    pub peer_node_key: &'a str,
    pub peer_in_group: bool,
    pub lane: Lane,
    pub workspace_id: &'a str,
    pub current_policy: IntendedLanePolicy,
    pub proposed_policy: IntendedLanePolicy,
    pub memories: &'a [MemoryView<'a>],
    pub sample_strategy: SampleStrategy,
    /// Caller-requested cap on preview-row count. Internally clamped
    /// to [`LANE_GRANT_PREVIEW_MAX_LIMIT`].
    pub limit: usize,
    /// Names of redaction classes that the upstream pipeline already
    /// applied (e.g. `["api_key", "jwt", "tailscale_metadata"]`).
    /// Reported into the output unchanged.
    pub redaction_rules: &'a [String],
    /// Seed for [`SampleStrategy::Random`]. Pinned by the test layer
    /// and by `--seed` on the future CLI surface for reproducibility.
    pub sample_random_seed: u64,
}

// ============================================================================
// Output shapes (camelCase serde for direct envelope emission)
// ============================================================================

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicySnapshot {
    pub lane: String,
    pub decision: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreviewRow {
    #[serde(rename = "memoryId")]
    pub memory_id: String,
    pub level: String,
    pub kind: String,
    #[serde(rename = "contentPreview")]
    pub content_preview: String,
    pub tags: Vec<String>,
    #[serde(rename = "trustClass")]
    pub trust_class: TrustClass,
    #[serde(rename = "hasSensitiveTags")]
    pub has_sensitive_tags: bool,
    #[serde(rename = "redactedFields")]
    pub redacted_fields: Vec<String>,
    #[serde(rename = "wouldExposeUnderProposedPolicy")]
    pub would_expose_under_proposed_policy: bool,
}

/// Caution kind vocabulary. Held as `&'static str` constants so a
/// downstream code-taxonomy gate can statically index them.
pub mod caution_kinds {
    pub const HIGH_TRUST_CLASS_EXPOSURE: &str = "high_trust_class_exposure";
    pub const LARGE_VOLUME_EXPOSURE: &str = "large_volume_exposure";
    pub const SENSITIVE_TAGS_IN_EXPOSURE: &str = "sensitive_tags_in_exposure";
    pub const TOMBSTONED_IN_EXPOSURE: &str = "tombstoned_in_exposure";
    pub const REDACTION_ACTIVE: &str = "redaction_active";
    pub const PEER_NOT_IN_GROUP: &str = "peer_not_in_group";
    pub const LANE_ALREADY_GRANTED: &str = "lane_already_granted";
}

/// One UX hazard surfaced by the preview. Severity is one of
/// `"info" | "warning"`; an `"error"` severity would imply the preview
/// itself failed, which is not a path this pure-decision module takes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Caution {
    pub kind: String,
    pub message: String,
    pub severity: String,
}

/// Schema-shaped envelope ready for emission via the renderer or MCP.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LaneGrantPreview {
    pub schema: &'static str,
    #[serde(rename = "peerNodeKey")]
    pub peer_node_key: String,
    pub lane: String,
    #[serde(rename = "workspaceId")]
    pub workspace_id: String,
    #[serde(rename = "currentPolicy")]
    pub current_policy: PolicySnapshot,
    #[serde(rename = "proposedPolicy")]
    pub proposed_policy: PolicySnapshot,
    #[serde(rename = "affectedMemoryCount")]
    pub affected_memory_count: u64,
    #[serde(rename = "redactedFromExposureCount")]
    pub redacted_from_exposure_count: u64,
    #[serde(rename = "previewSampleStrategy")]
    pub preview_sample_strategy: String,
    #[serde(rename = "previewSample")]
    pub preview_sample: Vec<PreviewRow>,
    #[serde(rename = "redactionRulesApplied")]
    pub redaction_rules_applied: Vec<String>,
    pub cautions: Vec<Caution>,
}

// ============================================================================
// Core decision function
// ============================================================================

/// Compute the lane-grant preview envelope. Pure function — caller
/// supplies all facts; no DB queries, no audit emission, no I/O.
///
/// Algorithm (read-only invariant — load-bearing):
/// 1. Effective limit = `min(input.limit, MAX_LIMIT)`, or DEFAULT_LIMIT
///    when caller passed 0.
/// 2. Partition memories into "would expose" (proposed policy is
///    [`LaneDecision::Allow`] and not blocked by redaction class AND
///    not tombstoned) and "would-not-expose" (the residual).
/// 3. `affectedMemoryCount = |would_expose|`.
/// 4. `redactedFromExposureCount = #memories where
///    blocked_by_redaction_class && proposed_policy_allows_lane`.
/// 5. Apply [`SampleStrategy`] to `would_expose`, clip to the
///    effective limit, project to [`PreviewRow`]s.
/// 6. Run the caution detection rules across the entire memory set
///    (not just the sample) so volume / tag / trust signals don't
///    depend on sampling.
#[must_use]
pub fn compute_lane_grant_preview(input: &LaneGrantPreviewInput<'_>) -> LaneGrantPreview {
    let effective_limit = effective_limit(input.limit);
    let current_decision = input.lane.decision_in(&input.current_policy);
    let proposed_decision = input.lane.decision_in(&input.proposed_policy);
    let proposed_allows = proposed_decision == LaneDecision::Allow;

    let mut would_expose: Vec<&MemoryView<'_>> = Vec::with_capacity(input.memories.len());
    let mut tombstoned_blocked = 0_u64;
    let mut redacted_blocked = 0_u64;

    for memory in input.memories {
        let exposable = proposed_allows && !memory.is_tombstoned && !memory.blocked_by_redaction_class;
        if exposable {
            would_expose.push(memory);
            continue;
        }
        if proposed_allows && memory.is_tombstoned {
            tombstoned_blocked += 1;
        }
        if proposed_allows && memory.blocked_by_redaction_class {
            redacted_blocked += 1;
        }
    }

    let affected_memory_count = would_expose.len() as u64;

    sort_sample(&mut would_expose, input.sample_strategy, input.sample_random_seed);
    let sample_rows: Vec<PreviewRow> = would_expose
        .iter()
        .take(effective_limit)
        .map(|memory| build_preview_row(memory, true))
        .collect();

    let cautions = collect_cautions(
        input,
        current_decision,
        proposed_decision,
        affected_memory_count,
        tombstoned_blocked,
        redacted_blocked,
    );

    LaneGrantPreview {
        schema: LANE_GRANT_PREVIEW_SCHEMA_V1,
        peer_node_key: input.peer_node_key.to_owned(),
        lane: input.lane.as_str().to_owned(),
        workspace_id: input.workspace_id.to_owned(),
        current_policy: PolicySnapshot {
            lane: input.lane.as_str().to_owned(),
            decision: current_decision.as_str().to_owned(),
        },
        proposed_policy: PolicySnapshot {
            lane: input.lane.as_str().to_owned(),
            decision: proposed_decision.as_str().to_owned(),
        },
        affected_memory_count,
        redacted_from_exposure_count: redacted_blocked,
        preview_sample_strategy: input.sample_strategy.as_str().to_owned(),
        preview_sample: sample_rows,
        redaction_rules_applied: input.redaction_rules.to_vec(),
        cautions,
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

fn effective_limit(requested: usize) -> usize {
    let baseline = if requested == 0 {
        LANE_GRANT_PREVIEW_DEFAULT_LIMIT
    } else {
        requested
    };
    baseline.min(LANE_GRANT_PREVIEW_MAX_LIMIT)
}

fn build_preview_row(memory: &MemoryView<'_>, would_expose: bool) -> PreviewRow {
    PreviewRow {
        memory_id: memory.memory_id.to_owned(),
        level: memory.level.to_owned(),
        kind: memory.kind.to_owned(),
        content_preview: truncate_chars(memory.content, LANE_GRANT_PREVIEW_CONTENT_PREVIEW_CHARS),
        tags: memory.tags.to_vec(),
        trust_class: memory.trust_class,
        has_sensitive_tags: memory_has_sensitive_tag(memory),
        redacted_fields: memory.redacted_fields.to_vec(),
        would_expose_under_proposed_policy: would_expose,
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    value.chars().take(max_chars).collect()
}

fn memory_has_sensitive_tag(memory: &MemoryView<'_>) -> bool {
    let sensitive: BTreeSet<&str> = SENSITIVE_TAGS.iter().copied().collect();
    memory
        .tags
        .iter()
        .any(|tag| sensitive.contains(tag.as_str()))
}

fn sort_sample(items: &mut [&MemoryView<'_>], strategy: SampleStrategy, seed: u64) {
    match strategy {
        SampleStrategy::HighestTrust => {
            items.sort_by_key(|memory| {
                (Reverse(memory.trust_class.trust_score()), memory.memory_id)
            });
        }
        SampleStrategy::MostRecent => {
            items.sort_by_key(|memory| (Reverse(memory.created_at_secs), memory.memory_id));
        }
        SampleStrategy::Random => {
            items.sort_by_key(|memory| deterministic_random_key(memory.memory_id, seed));
        }
    }
}

/// Deterministic per-row sort key for [`SampleStrategy::Random`]. Uses
/// blake3 of `(seed, memory_id)` so the same seed reproduces the same
/// ordering across runs (and across machines). Not cryptographic
/// strength is required — only deterministic and well-mixed.
fn deterministic_random_key(memory_id: &str, seed: u64) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&seed.to_le_bytes());
    hasher.update(memory_id.as_bytes());
    let mut out = [0_u8; 16];
    out.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    out
}

fn collect_cautions(
    input: &LaneGrantPreviewInput<'_>,
    current_decision: LaneDecision,
    _proposed_decision: LaneDecision,
    affected_memory_count: u64,
    tombstoned_blocked: u64,
    redacted_blocked: u64,
) -> Vec<Caution> {
    let mut cautions = Vec::new();

    if !input.peer_in_group {
        cautions.push(Caution {
            kind: caution_kinds::PEER_NOT_IN_GROUP.to_owned(),
            message: format!(
                "peer {} is not in the workspace's auto-enrolled peer-group; the preview still runs but the peer would not actually receive data until enrolled",
                input.peer_node_key
            ),
            severity: "warning".to_owned(),
        });
    }

    if current_decision == LaneDecision::Allow {
        cautions.push(Caution {
            kind: caution_kinds::LANE_ALREADY_GRANTED.to_owned(),
            message: format!(
                "lane '{}' is already granted in the current policy; this preview shows what is currently exposed",
                input.lane.as_str()
            ),
            severity: "info".to_owned(),
        });
    }

    let mut high_trust_exposure_count: u64 = 0;
    let mut sensitive_tag_exposure_count: u64 = 0;
    let proposed_allows = input.lane.decision_in(&input.proposed_policy) == LaneDecision::Allow;
    for memory in input.memories {
        let would_expose = proposed_allows
            && !memory.is_tombstoned
            && !memory.blocked_by_redaction_class;
        if !would_expose {
            continue;
        }
        if memory.trust_class.is_high_trust() {
            high_trust_exposure_count += 1;
        }
        if memory_has_sensitive_tag(memory) {
            sensitive_tag_exposure_count += 1;
        }
    }

    if high_trust_exposure_count > 0 {
        cautions.push(Caution {
            kind: caution_kinds::HIGH_TRUST_CLASS_EXPOSURE.to_owned(),
            message: format!(
                "{high_trust_exposure_count} memor{plural} with trust_class=human_explicit would be exposed; these are the user's directly-authored rules",
                plural = if high_trust_exposure_count == 1 { "y" } else { "ies" }
            ),
            severity: "warning".to_owned(),
        });
    }

    if affected_memory_count > LANE_GRANT_PREVIEW_LARGE_VOLUME_THRESHOLD {
        cautions.push(Caution {
            kind: caution_kinds::LARGE_VOLUME_EXPOSURE.to_owned(),
            message: format!(
                "{affected_memory_count} memories would be exposed (>{LANE_GRANT_PREVIEW_LARGE_VOLUME_THRESHOLD}); the workspace may be larger than expected"
            ),
            severity: "warning".to_owned(),
        });
    }

    if sensitive_tag_exposure_count > 0 {
        cautions.push(Caution {
            kind: caution_kinds::SENSITIVE_TAGS_IN_EXPOSURE.to_owned(),
            message: format!(
                "{sensitive_tag_exposure_count} memor{plural} tagged secret/private/personal/internal would be exposed; tag-driven scope filtering is the user's main lever to hide things",
                plural = if sensitive_tag_exposure_count == 1 { "y" } else { "ies" }
            ),
            severity: "warning".to_owned(),
        });
    }

    if tombstoned_blocked > 0 {
        cautions.push(Caution {
            kind: caution_kinds::TOMBSTONED_IN_EXPOSURE.to_owned(),
            message: format!(
                "{tombstoned_blocked} tombstoned memor{plural} would not be exposed; tombstoned status is honored",
                plural = if tombstoned_blocked == 1 { "y" } else { "ies" }
            ),
            severity: "info".to_owned(),
        });
    }

    if redacted_blocked > 0 {
        cautions.push(Caution {
            kind: caution_kinds::REDACTION_ACTIVE.to_owned(),
            message: format!(
                "{redacted_blocked} memor{plural} would have specific fields redacted under existing redaction-class rules",
                plural = if redacted_blocked == 1 { "y" } else { "ies" }
            ),
            severity: "info".to_owned(),
        });
    }

    cautions
}

// ============================================================================
// Inline tests (AGENTS.md L300-302 / bd-3usjw.62 Rule 7)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::auto_enrollment_safety::IntendedLanePolicy;

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_owned()).collect()
    }

    fn empty_strings() -> Vec<String> {
        Vec::new()
    }

    fn body_grant_proposed() -> IntendedLanePolicy {
        let mut policy = IntendedLanePolicy::conservative_default();
        policy.body = LaneDecision::Allow;
        policy
    }

    fn build_memory<'a>(
        memory_id: &'a str,
        trust_class: TrustClass,
        tag_storage: &'a [String],
        created_at_secs: i64,
        is_tombstoned: bool,
        blocked_by_redaction_class: bool,
        redacted_field_storage: &'a [String],
    ) -> MemoryView<'a> {
        MemoryView {
            memory_id,
            level: "memory",
            kind: "fact",
            content: "example content body that the peer would see if body lane is granted",
            tags: tag_storage,
            trust_class,
            redacted_fields: redacted_field_storage,
            created_at_secs,
            is_tombstoned,
            blocked_by_redaction_class,
        }
    }

    // ---- Lane <-> IntendedLanePolicy decision lookup -----------------------

    #[test]
    fn lane_decision_lookup_matches_policy_fields() {
        let policy = IntendedLanePolicy {
            metadata: LaneDecision::Allow,
            body: LaneDecision::Quarantine,
            embedding: LaneDecision::Deny,
            graph_link: LaneDecision::Deny,
            curation_signal: LaneDecision::Allow,
            revision_notice: LaneDecision::Allow,
        };
        assert_eq!(Lane::Metadata.decision_in(&policy), LaneDecision::Allow);
        assert_eq!(Lane::Body.decision_in(&policy), LaneDecision::Quarantine);
        assert_eq!(Lane::Embedding.decision_in(&policy), LaneDecision::Deny);
        assert_eq!(Lane::GraphLink.decision_in(&policy), LaneDecision::Deny);
        assert_eq!(
            Lane::CurationSignal.decision_in(&policy),
            LaneDecision::Allow
        );
        assert_eq!(Lane::RevisionNotice.decision_in(&policy), LaneDecision::Allow);
    }

    // ---- effective_limit clamping ------------------------------------------

    #[test]
    fn effective_limit_falls_back_to_default_when_zero() {
        assert_eq!(effective_limit(0), LANE_GRANT_PREVIEW_DEFAULT_LIMIT);
    }

    #[test]
    fn effective_limit_honors_requested_below_max() {
        assert_eq!(effective_limit(50), 50);
    }

    #[test]
    fn effective_limit_clamps_to_max() {
        assert_eq!(
            effective_limit(usize::MAX),
            LANE_GRANT_PREVIEW_MAX_LIMIT
        );
    }

    // ---- truncate_chars (multibyte-safe) -----------------------------------

    #[test]
    fn truncate_chars_short_returns_input_unchanged() {
        assert_eq!(truncate_chars("hello", 100), "hello");
    }

    #[test]
    fn truncate_chars_long_truncates_to_char_count_not_byte_count() {
        // 4-byte UTF-8 chars (rocket); each is one char, so 5 chars max
        let s = "🚀🚀🚀🚀🚀🚀";
        assert_eq!(truncate_chars(s, 5).chars().count(), 5);
    }

    // ---- Pure compute: deny → allow lane shows everything ------------------

    #[test]
    fn body_deny_to_allow_with_non_tombstoned_non_blocked_memories_exposes_all() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories = [
            build_memory(
                "m1",
                TrustClass::AgentProposed,
                &no_tags,
                1_000_000,
                false,
                false,
                &no_redacted,
            ),
            build_memory(
                "m2",
                TrustClass::AgentProposed,
                &no_tags,
                2_000_000,
                false,
                false,
                &no_redacted,
            ),
        ];
        let redaction_rules = empty_strings();
        let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::Random,
            limit: 25,
            redaction_rules: &redaction_rules,
            sample_random_seed: 42,
        });

        assert_eq!(preview.affected_memory_count, 2);
        assert_eq!(preview.redacted_from_exposure_count, 0);
        assert_eq!(preview.preview_sample.len(), 2);
        assert_eq!(preview.lane, "body");
        assert_eq!(preview.current_policy.decision, "deny");
        assert_eq!(preview.proposed_policy.decision, "allow");
        assert!(preview
            .preview_sample
            .iter()
            .all(|row| row.would_expose_under_proposed_policy));
    }

    // ---- Read-only invariant: tombstoned + blocked are excluded ------------

    #[test]
    fn tombstoned_and_redaction_blocked_memories_are_excluded_from_exposure() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories = [
            build_memory(
                "live",
                TrustClass::AgentProposed,
                &no_tags,
                1,
                false,
                false,
                &no_redacted,
            ),
            build_memory(
                "tomb",
                TrustClass::AgentProposed,
                &no_tags,
                1,
                true,
                false,
                &no_redacted,
            ),
            build_memory(
                "blocked",
                TrustClass::AgentProposed,
                &no_tags,
                1,
                false,
                true,
                &no_redacted,
            ),
        ];
        let redaction_rules = empty_strings();
        let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::Random,
            limit: 25,
            redaction_rules: &redaction_rules,
            sample_random_seed: 42,
        });

        assert_eq!(preview.affected_memory_count, 1);
        assert_eq!(preview.redacted_from_exposure_count, 1);
        assert!(preview
            .preview_sample
            .iter()
            .all(|row| row.memory_id == "live"));

        let kinds: BTreeSet<&str> = preview.cautions.iter().map(|c| c.kind.as_str()).collect();
        assert!(kinds.contains(caution_kinds::TOMBSTONED_IN_EXPOSURE));
        assert!(kinds.contains(caution_kinds::REDACTION_ACTIVE));
    }

    // ---- Caution: high trust exposure --------------------------------------

    #[test]
    fn high_trust_exposure_caution_fires_for_human_explicit() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories = [build_memory(
            "explicit-1",
            TrustClass::HumanExplicit,
            &no_tags,
            1,
            false,
            false,
            &no_redacted,
        )];
        let redaction_rules = empty_strings();
        let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::HighestTrust,
            limit: 25,
            redaction_rules: &redaction_rules,
            sample_random_seed: 42,
        });

        let kinds: BTreeSet<&str> = preview.cautions.iter().map(|c| c.kind.as_str()).collect();
        assert!(kinds.contains(caution_kinds::HIGH_TRUST_CLASS_EXPOSURE));
    }

    // ---- Caution: sensitive tag exposure -----------------------------------

    #[test]
    fn sensitive_tag_exposure_caution_fires_for_canonical_tags() {
        for sensitive_tag in SENSITIVE_TAGS {
            let tag_storage = tags(&[sensitive_tag]);
            let no_redacted = empty_strings();
            let memories = [build_memory(
                "m1",
                TrustClass::AgentProposed,
                &tag_storage,
                1,
                false,
                false,
                &no_redacted,
            )];
            let redaction_rules = empty_strings();
            let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
                peer_node_key: "nodekey:test",
                peer_in_group: true,
                lane: Lane::Body,
                workspace_id: "ws-1",
                current_policy: IntendedLanePolicy::conservative_default(),
                proposed_policy: body_grant_proposed(),
                memories: &memories,
                sample_strategy: SampleStrategy::Random,
                limit: 25,
                redaction_rules: &redaction_rules,
                sample_random_seed: 42,
            });

            let kinds: BTreeSet<&str> =
                preview.cautions.iter().map(|c| c.kind.as_str()).collect();
            assert!(
                kinds.contains(caution_kinds::SENSITIVE_TAGS_IN_EXPOSURE),
                "tag {sensitive_tag} should fire sensitive caution",
            );
            assert!(preview.preview_sample[0].has_sensitive_tags);
        }
    }

    // ---- Caution: peer not in group, lane already granted ------------------

    #[test]
    fn peer_not_in_group_caution_fires_with_explicit_severity() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories = [build_memory(
            "m1",
            TrustClass::AgentProposed,
            &no_tags,
            1,
            false,
            false,
            &no_redacted,
        )];
        let redaction_rules = empty_strings();
        let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:stranger",
            peer_in_group: false,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::Random,
            limit: 25,
            redaction_rules: &redaction_rules,
            sample_random_seed: 42,
        });

        let caution = preview
            .cautions
            .iter()
            .find(|c| c.kind == caution_kinds::PEER_NOT_IN_GROUP)
            .expect("peer_not_in_group caution present");
        assert_eq!(caution.severity, "warning");
        assert!(caution.message.contains("nodekey:stranger"));
    }

    #[test]
    fn lane_already_granted_caution_fires_when_current_is_allow() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories = [build_memory(
            "m1",
            TrustClass::AgentProposed,
            &no_tags,
            1,
            false,
            false,
            &no_redacted,
        )];
        // Current and proposed both grant body — informational case.
        let already_allow = body_grant_proposed();
        let redaction_rules = empty_strings();
        let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: already_allow,
            proposed_policy: already_allow,
            memories: &memories,
            sample_strategy: SampleStrategy::Random,
            limit: 25,
            redaction_rules: &redaction_rules,
            sample_random_seed: 42,
        });

        let caution = preview
            .cautions
            .iter()
            .find(|c| c.kind == caution_kinds::LANE_ALREADY_GRANTED)
            .expect("lane_already_granted caution present");
        assert_eq!(caution.severity, "info");
    }

    // ---- Sample strategy: HighestTrust orders human_explicit first ---------

    #[test]
    fn highest_trust_strategy_orders_human_explicit_before_agent_proposed() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories = [
            build_memory(
                "agent",
                TrustClass::AgentProposed,
                &no_tags,
                1,
                false,
                false,
                &no_redacted,
            ),
            build_memory(
                "explicit",
                TrustClass::HumanExplicit,
                &no_tags,
                1,
                false,
                false,
                &no_redacted,
            ),
            build_memory(
                "external",
                TrustClass::External,
                &no_tags,
                1,
                false,
                false,
                &no_redacted,
            ),
        ];
        let redaction_rules = empty_strings();
        let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::HighestTrust,
            limit: 25,
            redaction_rules: &redaction_rules,
            sample_random_seed: 42,
        });

        let order: Vec<&str> = preview
            .preview_sample
            .iter()
            .map(|row| row.memory_id.as_str())
            .collect();
        assert_eq!(order, vec!["explicit", "agent", "external"]);
    }

    // ---- Sample strategy: MostRecent orders by created_at desc -------------

    #[test]
    fn most_recent_strategy_orders_newest_first() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories = [
            build_memory(
                "old",
                TrustClass::AgentProposed,
                &no_tags,
                100,
                false,
                false,
                &no_redacted,
            ),
            build_memory(
                "new",
                TrustClass::AgentProposed,
                &no_tags,
                999_999,
                false,
                false,
                &no_redacted,
            ),
            build_memory(
                "middle",
                TrustClass::AgentProposed,
                &no_tags,
                5_000,
                false,
                false,
                &no_redacted,
            ),
        ];
        let redaction_rules = empty_strings();
        let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::MostRecent,
            limit: 25,
            redaction_rules: &redaction_rules,
            sample_random_seed: 42,
        });

        let order: Vec<&str> = preview
            .preview_sample
            .iter()
            .map(|row| row.memory_id.as_str())
            .collect();
        assert_eq!(order, vec!["new", "middle", "old"]);
    }

    // ---- Sample strategy: Random is deterministic for fixed seed ----------

    #[test]
    fn random_strategy_is_deterministic_for_fixed_seed() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories: Vec<MemoryView<'_>> = (0..20)
            .map(|i| {
                let id: &'static str = match i {
                    0 => "m00", 1 => "m01", 2 => "m02", 3 => "m03", 4 => "m04",
                    5 => "m05", 6 => "m06", 7 => "m07", 8 => "m08", 9 => "m09",
                    10 => "m10", 11 => "m11", 12 => "m12", 13 => "m13", 14 => "m14",
                    15 => "m15", 16 => "m16", 17 => "m17", 18 => "m18", _ => "m19",
                };
                build_memory(
                    id,
                    TrustClass::AgentProposed,
                    &no_tags,
                    i,
                    false,
                    false,
                    &no_redacted,
                )
            })
            .collect();
        let redaction_rules = empty_strings();

        let preview_a = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::Random,
            limit: 5,
            redaction_rules: &redaction_rules,
            sample_random_seed: 42,
        });
        let preview_b = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::Random,
            limit: 5,
            redaction_rules: &redaction_rules,
            sample_random_seed: 42,
        });

        let ids_a: Vec<&str> = preview_a
            .preview_sample
            .iter()
            .map(|row| row.memory_id.as_str())
            .collect();
        let ids_b: Vec<&str> = preview_b
            .preview_sample
            .iter()
            .map(|row| row.memory_id.as_str())
            .collect();
        assert_eq!(ids_a, ids_b, "same seed must produce same ordering");
    }

    #[test]
    fn random_strategy_different_seed_produces_different_ordering() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories: Vec<MemoryView<'_>> = (0..20)
            .map(|i| {
                let id: &'static str = match i {
                    0 => "m00", 1 => "m01", 2 => "m02", 3 => "m03", 4 => "m04",
                    5 => "m05", 6 => "m06", 7 => "m07", 8 => "m08", 9 => "m09",
                    10 => "m10", 11 => "m11", 12 => "m12", 13 => "m13", 14 => "m14",
                    15 => "m15", 16 => "m16", 17 => "m17", 18 => "m18", _ => "m19",
                };
                build_memory(
                    id,
                    TrustClass::AgentProposed,
                    &no_tags,
                    i,
                    false,
                    false,
                    &no_redacted,
                )
            })
            .collect();
        let redaction_rules = empty_strings();

        let preview_a = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::Random,
            limit: 20,
            redaction_rules: &redaction_rules,
            sample_random_seed: 1,
        });
        let preview_b = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::Random,
            limit: 20,
            redaction_rules: &redaction_rules,
            sample_random_seed: 2,
        });

        let ids_a: Vec<&str> = preview_a
            .preview_sample
            .iter()
            .map(|row| row.memory_id.as_str())
            .collect();
        let ids_b: Vec<&str> = preview_b
            .preview_sample
            .iter()
            .map(|row| row.memory_id.as_str())
            .collect();
        assert_ne!(ids_a, ids_b, "different seeds should rarely match");
    }

    // ---- Large volume caution fires above threshold ------------------------

    #[test]
    fn large_volume_exposure_caution_fires_above_threshold() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories: Vec<MemoryView<'_>> = (0..1500)
            .map(|i| {
                // Leak a deterministic but unique id; for >1000 elements we
                // need to allocate string storage outside the loop to keep
                // the borrow checker happy in a Vec<MemoryView<'a>>.
                let id_ref: &'static str = Box::leak(format!("m{i:04}").into_boxed_str());
                build_memory(
                    id_ref,
                    TrustClass::AgentProposed,
                    &no_tags,
                    i,
                    false,
                    false,
                    &no_redacted,
                )
            })
            .collect();
        let redaction_rules = empty_strings();
        let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::Random,
            limit: 25,
            redaction_rules: &redaction_rules,
            sample_random_seed: 7,
        });

        assert_eq!(preview.affected_memory_count, 1500);
        assert_eq!(preview.preview_sample.len(), 25);

        let kinds: BTreeSet<&str> = preview.cautions.iter().map(|c| c.kind.as_str()).collect();
        assert!(kinds.contains(caution_kinds::LARGE_VOLUME_EXPOSURE));
    }

    // ---- Proposed policy still Deny → zero exposure, zero cautions ---------

    #[test]
    fn proposed_deny_yields_zero_exposure_and_minimal_cautions() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories = [build_memory(
            "m1",
            TrustClass::HumanExplicit,
            &no_tags,
            1,
            false,
            false,
            &no_redacted,
        )];
        let redaction_rules = empty_strings();
        let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            // Proposed leaves body=Deny (same as current); preview is a no-op.
            proposed_policy: IntendedLanePolicy::conservative_default(),
            memories: &memories,
            sample_strategy: SampleStrategy::Random,
            limit: 25,
            redaction_rules: &redaction_rules,
            sample_random_seed: 42,
        });

        assert_eq!(preview.affected_memory_count, 0);
        assert_eq!(preview.preview_sample.len(), 0);
        // No high_trust_class_exposure caution because nothing is exposed.
        let kinds: BTreeSet<&str> = preview.cautions.iter().map(|c| c.kind.as_str()).collect();
        assert!(!kinds.contains(caution_kinds::HIGH_TRUST_CLASS_EXPOSURE));
        assert!(!kinds.contains(caution_kinds::LARGE_VOLUME_EXPOSURE));
    }

    // ---- Schema constant is the documented version -------------------------

    #[test]
    fn schema_constant_is_documented_version() {
        assert_eq!(
            LANE_GRANT_PREVIEW_SCHEMA_V1,
            "ee.mesh.lane_grant_preview.v1"
        );
    }
}
