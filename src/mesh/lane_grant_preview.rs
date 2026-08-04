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
//! `ee.mesh.lane_grant_preview.v2` schema, ready for any renderer.
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
#[cfg(test)]
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::mesh::auto_enrollment_safety::{IntendedLanePolicy, LaneDecision};
use crate::models::TrustClass;

/// JSON schema identifier for the lane-grant preview output. Held as the
/// source-of-truth constant so the renderer and the schema-lifecycle
/// drift gate agree.
pub const LANE_GRANT_PREVIEW_SCHEMA_V2: &str = "ee.mesh.lane_grant_preview.v2";

/// Copy contract bound into every authenticated approval snapshot. Copy
/// changes are consent changes: a token issued for older operator wording may
/// not authorize a mutation rendered with newer wording.
pub const LANE_GRANT_PREVIEW_COPY_VERSION: &str = "ee.mesh.lane_grant_preview.copy.v1";

/// Versioned opaque-handle adapter persisted with the grant. T2.2/T3.1 can
/// migrate its private representation when stable node identities land; the
/// public preview intentionally exposes only this version and the peer ID.
pub const LANE_GRANT_TARGET_ADAPTER_VERSION: &str = "ee.mesh.grant_target.v1";

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
    /// Canonical wire string used in `ee.mesh.lane_grant_preview.v2`.
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

fn trust_score(trust_class: TrustClass) -> u8 {
    match trust_class {
        TrustClass::HumanExplicit => 5,
        TrustClass::AgentValidated => 4,
        TrustClass::AgentAssertion => 3,
        TrustClass::CassEvidence => 2,
        TrustClass::LegacyImport => 1,
    }
}

fn is_high_trust(trust_class: TrustClass) -> bool {
    matches!(trust_class, TrustClass::HumanExplicit)
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
    pub generation: String,
    pub lane: String,
    pub decision: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantTargetSnapshot {
    pub adapter_version: String,
    pub peer_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateRevision {
    pub memory_id: String,
    pub revision_id: String,
}

/// Additional state that turns the pure visibility calculation into the
/// canonical approval snapshot. The legacy wrapper uses deterministic
/// placeholder generations; DB-backed callers must provide real values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaneGrantApprovalContext<'a> {
    pub target_peer_id: &'a str,
    pub grant_generation: u64,
    /// Monotonic workspace mutation generation used as the candidate-set
    /// revision fence. Memory rows are not universally immutable yet: content,
    /// trust, tombstones, and tags can change in place. The workspace generation
    /// advances for each of those source mutations, so incorporating it into
    /// every revision pin makes even an unsampled change stale without exposing
    /// a body/content hash in the public preview.
    pub candidate_revision_generation: u64,
    pub current_policy_generation: &'a str,
    pub proposed_policy_generation: &'a str,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalTokenProjection {
    pub schema: String,
    pub sensitive: bool,
    pub bearer: String,
    pub expires_at: String,
    pub external_recorder_residual: String,
}

impl std::fmt::Debug for ApprovalTokenProjection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApprovalTokenProjection")
            .field("schema", &self.schema)
            .field("sensitive", &self.sensitive)
            .field("bearer", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field(
                "external_recorder_residual",
                &self.external_recorder_residual,
            )
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreviewRow {
    #[serde(rename = "memoryId")]
    pub memory_id: String,
    #[serde(rename = "revisionId")]
    pub revision_id: String,
    pub level: String,
    pub kind: String,
    #[serde(rename = "contentPreview")]
    pub content_preview: String,
    pub tags: Vec<String>,
    #[serde(rename = "trustClass")]
    pub trust_class: String,
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
    #[serde(rename = "copyVersion")]
    pub copy_version: &'static str,
    #[serde(rename = "workspaceId")]
    pub workspace_id: String,
    pub target: GrantTargetSnapshot,
    pub lane: String,
    #[serde(rename = "grantGeneration")]
    pub grant_generation: u64,
    #[serde(rename = "currentPolicy")]
    pub current_policy: PolicySnapshot,
    #[serde(rename = "proposedPolicy")]
    pub proposed_policy: PolicySnapshot,
    #[serde(rename = "candidateSet")]
    pub candidate_set: Vec<CandidateRevision>,
    #[serde(rename = "affectedMemoryCount")]
    pub affected_memory_count: u64,
    #[serde(rename = "redactedFromExposureCount")]
    pub redacted_from_exposure_count: u64,
    #[serde(rename = "previewSampleStrategy")]
    pub preview_sample_strategy: String,
    #[serde(rename = "previewSampleLimit")]
    pub preview_sample_limit: usize,
    #[serde(rename = "previewSample")]
    pub preview_sample: Vec<PreviewRow>,
    #[serde(rename = "redactionRulesApplied")]
    pub redaction_rules_applied: Vec<String>,
    #[serde(rename = "cautionCodes")]
    pub caution_codes: Vec<String>,
    pub cautions: Vec<Caution>,
    #[serde(rename = "approvalToken", skip_serializing_if = "Option::is_none")]
    pub approval_token: Option<ApprovalTokenProjection>,
}

impl LaneGrantPreview {
    /// Stable bytes authenticated by the approval token. The bearer projection
    /// is deliberately excluded so equal snapshots can receive unlinkable
    /// nonces without recursively authenticating their own token text.
    pub fn canonical_approval_snapshot_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut snapshot = self.clone();
        snapshot.approval_token = None;
        serde_json::to_vec(&snapshot)
    }
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
    compute_lane_grant_preview_with_context(
        input,
        &LaneGrantApprovalContext {
            target_peer_id: input.peer_node_key,
            grant_generation: 0,
            candidate_revision_generation: 0,
            current_policy_generation: "policy:unspecified",
            proposed_policy_generation: "policy:unspecified",
        },
    )
}

/// Compute the canonical v2 preview using DB/config-derived generation state.
#[must_use]
pub fn compute_lane_grant_preview_with_context(
    input: &LaneGrantPreviewInput<'_>,
    context: &LaneGrantApprovalContext<'_>,
) -> LaneGrantPreview {
    let effective_limit = effective_limit(input.limit);
    let current_decision = input.lane.decision_in(&input.current_policy);
    let proposed_decision = input.lane.decision_in(&input.proposed_policy);
    let proposed_allows = proposed_decision == LaneDecision::Allow;

    let mut would_expose: Vec<&MemoryView<'_>> = Vec::with_capacity(input.memories.len());
    let mut tombstoned_blocked = 0_u64;
    let mut redacted_blocked = 0_u64;

    for memory in input.memories {
        let exposable =
            proposed_allows && !memory.is_tombstoned && !memory.blocked_by_redaction_class;
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

    sort_sample(
        &mut would_expose,
        input.sample_strategy,
        input.sample_random_seed,
    );
    let sample_rows: Vec<PreviewRow> = would_expose
        .iter()
        .take(effective_limit)
        .map(|memory| build_preview_row(memory, true, context.candidate_revision_generation))
        .collect();

    let cautions = collect_cautions(
        input,
        current_decision,
        proposed_decision,
        affected_memory_count,
        tombstoned_blocked,
        redacted_blocked,
    );

    let mut candidate_set = input
        .memories
        .iter()
        .map(|memory| CandidateRevision {
            memory_id: memory.memory_id.to_owned(),
            revision_id: candidate_revision_id(
                memory.memory_id,
                context.candidate_revision_generation,
            ),
        })
        .collect::<Vec<_>>();
    candidate_set.sort();
    let caution_codes = cautions.iter().map(|item| item.kind.clone()).collect();

    LaneGrantPreview {
        schema: LANE_GRANT_PREVIEW_SCHEMA_V2,
        copy_version: LANE_GRANT_PREVIEW_COPY_VERSION,
        workspace_id: input.workspace_id.to_owned(),
        target: GrantTargetSnapshot {
            adapter_version: LANE_GRANT_TARGET_ADAPTER_VERSION.to_owned(),
            peer_id: context.target_peer_id.to_owned(),
        },
        lane: input.lane.as_str().to_owned(),
        grant_generation: context.grant_generation,
        current_policy: PolicySnapshot {
            generation: context.current_policy_generation.to_owned(),
            lane: input.lane.as_str().to_owned(),
            decision: current_decision.as_str().to_owned(),
        },
        proposed_policy: PolicySnapshot {
            generation: context.proposed_policy_generation.to_owned(),
            lane: input.lane.as_str().to_owned(),
            decision: proposed_decision.as_str().to_owned(),
        },
        candidate_set,
        affected_memory_count,
        redacted_from_exposure_count: redacted_blocked,
        preview_sample_strategy: input.sample_strategy.as_str().to_owned(),
        preview_sample_limit: effective_limit,
        preview_sample: sample_rows,
        redaction_rules_applied: input.redaction_rules.to_vec(),
        caution_codes,
        cautions,
        approval_token: None,
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

fn build_preview_row(
    memory: &MemoryView<'_>,
    would_expose: bool,
    candidate_revision_generation: u64,
) -> PreviewRow {
    PreviewRow {
        memory_id: memory.memory_id.to_owned(),
        revision_id: candidate_revision_id(memory.memory_id, candidate_revision_generation),
        level: memory.level.to_owned(),
        kind: memory.kind.to_owned(),
        content_preview: truncate_chars(memory.content, LANE_GRANT_PREVIEW_CONTENT_PREVIEW_CHARS),
        tags: memory.tags.to_vec(),
        trust_class: memory.trust_class.as_str().to_owned(),
        has_sensitive_tags: memory_has_sensitive_tag(memory),
        redacted_fields: memory.redacted_fields.to_vec(),
        would_expose_under_proposed_policy: would_expose,
    }
}

fn candidate_revision_id(memory_id: &str, candidate_revision_generation: u64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.mesh.lane_grant.candidate_revision.v1");
    hasher.update(&(memory_id.len() as u64).to_le_bytes());
    hasher.update(memory_id.as_bytes());
    hasher.update(&candidate_revision_generation.to_le_bytes());
    format!("revwg1_{}", hasher.finalize().to_hex())
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    value.chars().take(max_chars).collect()
}

fn memory_has_sensitive_tag(memory: &MemoryView<'_>) -> bool {
    memory.tags.iter().any(|tag| tag_has_sensitive_token(tag))
}

fn tag_has_sensitive_token(tag: &str) -> bool {
    tag.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .any(|token| {
            SENSITIVE_TAGS
                .iter()
                .any(|sensitive| token.eq_ignore_ascii_case(sensitive))
        })
}

fn sort_sample(items: &mut [&MemoryView<'_>], strategy: SampleStrategy, seed: u64) {
    match strategy {
        SampleStrategy::HighestTrust => {
            items
                .sort_by_key(|memory| (Reverse(trust_score(memory.trust_class)), memory.memory_id));
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
        let would_expose =
            proposed_allows && !memory.is_tombstoned && !memory.blocked_by_redaction_class;
        if !would_expose {
            continue;
        }
        if is_high_trust(memory.trust_class) {
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

    let field_redacted_memory_count = input
        .memories
        .iter()
        .filter(|memory| !memory.redacted_fields.is_empty())
        .count() as u64;
    if redacted_blocked > 0 || field_redacted_memory_count > 0 {
        let message = match (redacted_blocked, field_redacted_memory_count) {
            (blocked, 0) => format!(
                "{blocked} memor{plural} would not be exposed because existing redaction-class rules block that lane",
                plural = if blocked == 1 { "y" } else { "ies" }
            ),
            (0, field_redacted) => format!(
                "{field_redacted} memor{plural} had sensitive fields redacted before preview or exposure; the listed redaction rules remain active",
                plural = if field_redacted == 1 { "y" } else { "ies" }
            ),
            (blocked, field_redacted) => format!(
                "{blocked} memor{blocked_plural} would not be exposed because redaction-class rules block the lane, and {field_redacted} memor{field_plural} had sensitive fields redacted before preview or exposure",
                blocked_plural = if blocked == 1 { "y" } else { "ies" },
                field_plural = if field_redacted == 1 { "y" } else { "ies" },
            ),
        };
        cautions.push(Caution {
            kind: caution_kinds::REDACTION_ACTIVE.to_owned(),
            message,
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

    #[test]
    fn approval_token_debug_redacts_bearer() {
        let token = ApprovalTokenProjection {
            schema: "ee.mesh.approval_token.v1".to_owned(),
            sensitive: true,
            bearer: "eeap1_secret-bearer-material".to_owned(),
            expires_at: "2026-08-04T08:15:00Z".to_owned(),
            external_recorder_residual: "External logs may retain this token.".to_owned(),
        };

        let rendered = format!("{token:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("secret-bearer-material"));
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
        assert_eq!(
            Lane::RevisionNotice.decision_in(&policy),
            LaneDecision::Allow
        );
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
        assert_eq!(effective_limit(usize::MAX), LANE_GRANT_PREVIEW_MAX_LIMIT);
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
                TrustClass::AgentAssertion,
                &no_tags,
                1_000_000,
                false,
                false,
                &no_redacted,
            ),
            build_memory(
                "m2",
                TrustClass::AgentAssertion,
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
        assert!(
            preview
                .preview_sample
                .iter()
                .all(|row| row.would_expose_under_proposed_policy)
        );
    }

    #[test]
    fn workspace_generation_revision_pin_fences_unsampled_candidate_mutation() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories = [
            build_memory(
                "sampled",
                TrustClass::AgentAssertion,
                &no_tags,
                2,
                false,
                false,
                &no_redacted,
            ),
            build_memory(
                "unsampled",
                TrustClass::AgentAssertion,
                &no_tags,
                1,
                false,
                false,
                &no_redacted,
            ),
        ];
        let redaction_rules = empty_strings();
        let input = LaneGrantPreviewInput {
            peer_node_key: "nodekey:test",
            peer_in_group: true,
            lane: Lane::Body,
            workspace_id: "ws-1",
            current_policy: IntendedLanePolicy::conservative_default(),
            proposed_policy: body_grant_proposed(),
            memories: &memories,
            sample_strategy: SampleStrategy::MostRecent,
            limit: 1,
            redaction_rules: &redaction_rules,
            sample_random_seed: 0,
        };
        let before = compute_lane_grant_preview_with_context(
            &input,
            &LaneGrantApprovalContext {
                target_peer_id: "peer-1",
                grant_generation: 3,
                candidate_revision_generation: 41,
                current_policy_generation: "policy-current",
                proposed_policy_generation: "policy-proposed",
            },
        );
        let after_unsampled_mutation = compute_lane_grant_preview_with_context(
            &input,
            &LaneGrantApprovalContext {
                target_peer_id: "peer-1",
                grant_generation: 3,
                candidate_revision_generation: 42,
                current_policy_generation: "policy-current",
                proposed_policy_generation: "policy-proposed",
            },
        );

        assert_eq!(before.preview_sample.len(), 1);
        assert_eq!(before.preview_sample[0].memory_id, "sampled");
        let before_unsampled = before
            .candidate_set
            .iter()
            .find(|candidate| candidate.memory_id == "unsampled")
            .expect("complete candidate set includes unsampled memory");
        let after_unsampled = after_unsampled_mutation
            .candidate_set
            .iter()
            .find(|candidate| candidate.memory_id == "unsampled")
            .expect("complete candidate set still includes unsampled memory");
        assert_ne!(before_unsampled.revision_id, after_unsampled.revision_id);
        assert!(before_unsampled.revision_id.starts_with("revwg1_"));
        assert!(!before_unsampled.revision_id.contains("unsampled"));
        assert_ne!(
            before.canonical_approval_snapshot_bytes().unwrap(),
            after_unsampled_mutation
                .canonical_approval_snapshot_bytes()
                .unwrap(),
            "an unsampled source mutation must stale the authenticated snapshot",
        );
    }

    #[test]
    fn every_public_canonical_field_is_bound_but_bearer_projection_is_not() {
        let sensitive_tags = tags(&["private"]);
        let redacted_fields = tags(&["content:api_key"]);
        let memories = [build_memory(
            "m1",
            TrustClass::HumanExplicit,
            &sensitive_tags,
            1,
            false,
            false,
            &redacted_fields,
        )];
        let redaction_rules = tags(&["api_key"]);
        let already_allowed = body_grant_proposed();
        let base = compute_lane_grant_preview_with_context(
            &LaneGrantPreviewInput {
                peer_node_key: "nodekey:test",
                peer_in_group: false,
                lane: Lane::Body,
                workspace_id: "ws-1",
                current_policy: already_allowed,
                proposed_policy: already_allowed,
                memories: &memories,
                sample_strategy: SampleStrategy::Random,
                limit: 1,
                redaction_rules: &redaction_rules,
                sample_random_seed: 7,
            },
            &LaneGrantApprovalContext {
                target_peer_id: "peer-1",
                grant_generation: 3,
                candidate_revision_generation: 41,
                current_policy_generation: "policy-current",
                proposed_policy_generation: "policy-proposed",
            },
        );
        let canonical = base.canonical_approval_snapshot_bytes().unwrap();

        macro_rules! assert_field_drift {
            ($label:literal, $mutation:expr) => {{
                let mut changed = base.clone();
                $mutation(&mut changed);
                assert_ne!(
                    changed.canonical_approval_snapshot_bytes().unwrap(),
                    canonical,
                    "{} must be authenticated by the canonical snapshot",
                    $label,
                );
            }};
        }

        assert_field_drift!("schema", |value: &mut LaneGrantPreview| value.schema =
            "ee.mesh.lane_grant_preview.test");
        assert_field_drift!("copyVersion", |value: &mut LaneGrantPreview| value
            .copy_version =
            "ee.mesh.lane_grant_preview.copy.test");
        assert_field_drift!("workspaceId", |value: &mut LaneGrantPreview| value
            .workspace_id
            .push('x'));
        assert_field_drift!("target.adapterVersion", |value: &mut LaneGrantPreview| {
            value.target.adapter_version.push('x')
        });
        assert_field_drift!("target.peerId", |value: &mut LaneGrantPreview| value
            .target
            .peer_id
            .push('x'));
        assert_field_drift!("lane", |value: &mut LaneGrantPreview| value.lane.push('x'));
        assert_field_drift!("grantGeneration", |value: &mut LaneGrantPreview| value
            .grant_generation +=
            1);
        assert_field_drift!(
            "currentPolicy.generation",
            |value: &mut LaneGrantPreview| value.current_policy.generation.push('x')
        );
        assert_field_drift!("currentPolicy.lane", |value: &mut LaneGrantPreview| value
            .current_policy
            .lane
            .push('x'));
        assert_field_drift!("currentPolicy.decision", |value: &mut LaneGrantPreview| {
            value.current_policy.decision.push('x')
        });
        assert_field_drift!(
            "proposedPolicy.generation",
            |value: &mut LaneGrantPreview| value.proposed_policy.generation.push('x')
        );
        assert_field_drift!("proposedPolicy.lane", |value: &mut LaneGrantPreview| value
            .proposed_policy
            .lane
            .push('x'));
        assert_field_drift!("proposedPolicy.decision", |value: &mut LaneGrantPreview| {
            value.proposed_policy.decision.push('x')
        });
        assert_field_drift!("candidateSet.memoryId", |value: &mut LaneGrantPreview| {
            value.candidate_set[0].memory_id.push('x')
        });
        assert_field_drift!("candidateSet.revisionId", |value: &mut LaneGrantPreview| {
            value.candidate_set[0].revision_id.push('x')
        });
        assert_field_drift!("affectedMemoryCount", |value: &mut LaneGrantPreview| {
            value.affected_memory_count += 1
        });
        assert_field_drift!(
            "redactedFromExposureCount",
            |value: &mut LaneGrantPreview| value.redacted_from_exposure_count += 1
        );
        assert_field_drift!("previewSampleStrategy", |value: &mut LaneGrantPreview| {
            value.preview_sample_strategy.push('x')
        });
        assert_field_drift!("previewSampleLimit", |value: &mut LaneGrantPreview| {
            value.preview_sample_limit += 1
        });
        assert_field_drift!("previewSample.memoryId", |value: &mut LaneGrantPreview| {
            value.preview_sample[0].memory_id.push('x')
        });
        assert_field_drift!(
            "previewSample.revisionId",
            |value: &mut LaneGrantPreview| value.preview_sample[0].revision_id.push('x')
        );
        assert_field_drift!("previewSample.level", |value: &mut LaneGrantPreview| value
            .preview_sample[0]
            .level
            .push('x'));
        assert_field_drift!("previewSample.kind", |value: &mut LaneGrantPreview| value
            .preview_sample[0]
            .kind
            .push('x'));
        assert_field_drift!(
            "previewSample.contentPreview",
            |value: &mut LaneGrantPreview| value.preview_sample[0].content_preview.push('x')
        );
        assert_field_drift!("previewSample.tags", |value: &mut LaneGrantPreview| value
            .preview_sample[0]
            .tags
            .push("extra".to_owned()));
        assert_field_drift!(
            "previewSample.trustClass",
            |value: &mut LaneGrantPreview| value.preview_sample[0].trust_class.push('x')
        );
        assert_field_drift!(
            "previewSample.hasSensitiveTags",
            |value: &mut LaneGrantPreview| {
                let row = &mut value.preview_sample[0];
                row.has_sensitive_tags = !row.has_sensitive_tags;
            }
        );
        assert_field_drift!(
            "previewSample.redactedFields",
            |value: &mut LaneGrantPreview| value.preview_sample[0]
                .redacted_fields
                .push("tag:jwt".to_owned())
        );
        assert_field_drift!(
            "previewSample.wouldExposeUnderProposedPolicy",
            |value: &mut LaneGrantPreview| value.preview_sample[0]
                .would_expose_under_proposed_policy = false
        );
        assert_field_drift!("redactionRulesApplied", |value: &mut LaneGrantPreview| {
            value.redaction_rules_applied.push("jwt".to_owned())
        });
        assert_field_drift!("cautionCodes", |value: &mut LaneGrantPreview| value
            .caution_codes
            .push("extra".to_owned()));
        assert_field_drift!("cautions.kind", |value: &mut LaneGrantPreview| value
            .cautions[0]
            .kind
            .push('x'));
        assert_field_drift!("cautions.message", |value: &mut LaneGrantPreview| value
            .cautions[0]
            .message
            .push('x'));
        assert_field_drift!("cautions.severity", |value: &mut LaneGrantPreview| value
            .cautions[0]
            .severity
            .push('x'));

        let mut projected = base.clone();
        projected.approval_token = Some(ApprovalTokenProjection {
            schema: "ee.mesh.approval_token.v1".to_owned(),
            sensitive: true,
            bearer: "eeap1_redacted-test-bearer".to_owned(),
            expires_at: "2026-08-04T08:15:00Z".to_owned(),
            external_recorder_residual: "External logs may retain this token.".to_owned(),
        });
        assert_eq!(
            projected.canonical_approval_snapshot_bytes().unwrap(),
            canonical,
            "the bearer projection must not recursively authenticate itself",
        );
    }

    // ---- Read-only invariant: tombstoned + blocked are excluded ------------

    #[test]
    fn tombstoned_and_redaction_blocked_memories_are_excluded_from_exposure() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories = [
            build_memory(
                "live",
                TrustClass::AgentAssertion,
                &no_tags,
                1,
                false,
                false,
                &no_redacted,
            ),
            build_memory(
                "tomb",
                TrustClass::AgentAssertion,
                &no_tags,
                1,
                true,
                false,
                &no_redacted,
            ),
            build_memory(
                "blocked",
                TrustClass::AgentAssertion,
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
        assert!(
            preview
                .preview_sample
                .iter()
                .all(|row| row.memory_id == "live")
        );

        let kinds: BTreeSet<&str> = preview.cautions.iter().map(|c| c.kind.as_str()).collect();
        assert!(kinds.contains(caution_kinds::TOMBSTONED_IN_EXPOSURE));
        assert!(kinds.contains(caution_kinds::REDACTION_ACTIVE));
        let redaction_caution = preview
            .cautions
            .iter()
            .find(|caution| caution.kind == caution_kinds::REDACTION_ACTIVE)
            .expect("redaction_active caution present");
        assert!(redaction_caution.message.contains("would not be exposed"));
        assert!(
            redaction_caution
                .message
                .contains("redaction-class rules block that lane")
        );
    }

    #[test]
    fn field_level_redaction_emits_redaction_active_without_blocking_exposure() {
        let no_tags = empty_strings();
        let redacted_fields = tags(&["content:api_key"]);
        let memories = [build_memory(
            "redacted",
            TrustClass::AgentAssertion,
            &no_tags,
            1,
            false,
            false,
            &redacted_fields,
        )];
        let redaction_rules = tags(&["api_key"]);
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
        assert_eq!(preview.redacted_from_exposure_count, 0);
        assert_eq!(preview.preview_sample[0].redacted_fields, redacted_fields);
        let caution = preview
            .cautions
            .iter()
            .find(|caution| caution.kind == caution_kinds::REDACTION_ACTIVE)
            .expect("field redaction must emit redaction_active");
        assert!(caution.message.contains("had sensitive fields redacted"));
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
                TrustClass::AgentAssertion,
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

            let kinds: BTreeSet<&str> = preview.cautions.iter().map(|c| c.kind.as_str()).collect();
            assert!(
                kinds.contains(caution_kinds::SENSITIVE_TAGS_IN_EXPOSURE),
                "tag {sensitive_tag} should fire sensitive caution",
            );
            assert!(preview.preview_sample[0].has_sensitive_tags);
        }
    }

    #[test]
    fn sensitive_tag_exposure_caution_fires_for_case_and_scoped_tags() {
        let variants = [
            "Secret",
            "security:secret",
            "private-data",
            "personal_data",
            "INTERNAL",
        ];

        for variant in variants {
            let tag_storage = tags(&[variant]);
            let no_redacted = empty_strings();
            let memories = [build_memory(
                "m1",
                TrustClass::AgentAssertion,
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

            let kinds: BTreeSet<&str> = preview.cautions.iter().map(|c| c.kind.as_str()).collect();
            assert!(
                kinds.contains(caution_kinds::SENSITIVE_TAGS_IN_EXPOSURE),
                "tag {variant} should fire sensitive caution",
            );
            assert!(
                preview.preview_sample[0].has_sensitive_tags,
                "tag {variant} should mark the preview row sensitive",
            );
        }
    }

    #[test]
    fn sensitive_tag_exposure_caution_does_not_match_embedded_words() {
        let tag_storage = tags(&["nonsecret", "privately", "personality", "internalized"]);
        let no_redacted = empty_strings();
        let memories = [build_memory(
            "m1",
            TrustClass::AgentAssertion,
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

        let kinds: BTreeSet<&str> = preview.cautions.iter().map(|c| c.kind.as_str()).collect();
        assert!(!kinds.contains(caution_kinds::SENSITIVE_TAGS_IN_EXPOSURE));
        assert!(!preview.preview_sample[0].has_sensitive_tags);
    }

    // ---- Caution: peer not in group, lane already granted ------------------

    #[test]
    fn peer_not_in_group_caution_fires_with_explicit_severity() {
        let no_tags = empty_strings();
        let no_redacted = empty_strings();
        let memories = [build_memory(
            "m1",
            TrustClass::AgentAssertion,
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
            TrustClass::AgentAssertion,
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
                TrustClass::AgentAssertion,
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
                TrustClass::CassEvidence,
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
                TrustClass::AgentAssertion,
                &no_tags,
                100,
                false,
                false,
                &no_redacted,
            ),
            build_memory(
                "new",
                TrustClass::AgentAssertion,
                &no_tags,
                999_999,
                false,
                false,
                &no_redacted,
            ),
            build_memory(
                "middle",
                TrustClass::AgentAssertion,
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
                    0 => "m00",
                    1 => "m01",
                    2 => "m02",
                    3 => "m03",
                    4 => "m04",
                    5 => "m05",
                    6 => "m06",
                    7 => "m07",
                    8 => "m08",
                    9 => "m09",
                    10 => "m10",
                    11 => "m11",
                    12 => "m12",
                    13 => "m13",
                    14 => "m14",
                    15 => "m15",
                    16 => "m16",
                    17 => "m17",
                    18 => "m18",
                    _ => "m19",
                };
                build_memory(
                    id,
                    TrustClass::AgentAssertion,
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
                    0 => "m00",
                    1 => "m01",
                    2 => "m02",
                    3 => "m03",
                    4 => "m04",
                    5 => "m05",
                    6 => "m06",
                    7 => "m07",
                    8 => "m08",
                    9 => "m09",
                    10 => "m10",
                    11 => "m11",
                    12 => "m12",
                    13 => "m13",
                    14 => "m14",
                    15 => "m15",
                    16 => "m16",
                    17 => "m17",
                    18 => "m18",
                    _ => "m19",
                };
                build_memory(
                    id,
                    TrustClass::AgentAssertion,
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
                    TrustClass::AgentAssertion,
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
            LANE_GRANT_PREVIEW_SCHEMA_V2,
            "ee.mesh.lane_grant_preview.v2"
        );
    }
}
