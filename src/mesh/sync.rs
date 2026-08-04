//! SRR6.42 — selective sync filters, subscriptions, and sharing shapes.
//!
//! This module owns the pure decision surface for deciding which mesh export
//! candidates a peer subscription may receive. It deliberately has no database,
//! transport, or audit dependency: callers resolve candidate memories, load a
//! peer subscription and sharing profile, then hand everything here for a
//! deterministic preview or export filter pass.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Stable schema identifier for selective sync profile/config records.
pub const SELECTIVE_SYNC_PROFILE_SCHEMA_V1: &str = "ee.mesh.selective_sync_profile.v1";

/// Stable schema identifier for the local selective sync config surface.
pub const SELECTIVE_SYNC_CONFIG_SCHEMA_V1: &str = "ee.mesh.selective_sync_config.v1";

/// Stable schema identifier for status summaries derived from selective sync
/// config.
pub const SELECTIVE_SYNC_STATUS_SCHEMA_V1: &str = "ee.mesh.selective_sync_status.v1";

/// Stable schema identifier for dry-run preview output.
pub const SELECTIVE_SYNC_PREVIEW_SCHEMA_V1: &str = "ee.mesh.selective_sync_preview.v1";

/// Metadata-only starter profile id. Safe default: no body, embeddings, or
/// evidence-reference payloads cross the mesh boundary.
pub const STARTER_PROFILE_METADATA_ONLY: &str = "starter.metadata_only";

/// Starter profile id for evidence references without bodies or embeddings.
pub const STARTER_PROFILE_EVIDENCE_REFS: &str = "starter.evidence_refs";

/// Starter profile id for trusted bodies while embeddings stay local-only.
pub const STARTER_PROFILE_TRUSTED_BODIES: &str = "starter.trusted_bodies";

const DEFAULT_DENIED_TAGS: &[&str] = &["internal", "personal", "private", "secret"];

/// Material lane requested for one sync candidate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMaterialLane {
    Metadata,
    Body,
    Embedding,
    EvidenceRef,
    GraphLink,
    CurationSignal,
    RevisionNotice,
}

impl SyncMaterialLane {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Body => "body",
            Self::Embedding => "embedding",
            Self::EvidenceRef => "evidence_ref",
            Self::GraphLink => "graph_link",
            Self::CurationSignal => "curation_signal",
            Self::RevisionNotice => "revision_notice",
        }
    }
}

/// Trust class attached to the candidate memory.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncTrustClass {
    HumanExplicit,
    HumanRevised,
    PeerHumanAttested,
    AgentValidated,
    AgentProposed,
    External,
}

impl SyncTrustClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HumanExplicit => "human_explicit",
            Self::HumanRevised => "human_revised",
            Self::PeerHumanAttested => "peer_human_attested",
            Self::AgentValidated => "agent_validated",
            Self::AgentProposed => "agent_proposed",
            Self::External => "external",
        }
    }

    #[must_use]
    pub fn all() -> BTreeSet<Self> {
        [
            Self::HumanExplicit,
            Self::HumanRevised,
            Self::PeerHumanAttested,
            Self::AgentValidated,
            Self::AgentProposed,
            Self::External,
        ]
        .into_iter()
        .collect()
    }

    #[must_use]
    pub fn reviewed_or_validated() -> BTreeSet<Self> {
        [
            Self::HumanRevised,
            Self::PeerHumanAttested,
            Self::AgentValidated,
        ]
        .into_iter()
        .collect()
    }
}

/// Named material shape. Profiles combine a shape with content filters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectiveSyncShape {
    pub shape_id: String,
    pub material_lanes: BTreeSet<SyncMaterialLane>,
    pub include_evidence_refs: bool,
}

impl SelectiveSyncShape {
    #[must_use]
    pub fn new(
        shape_id: impl Into<String>,
        material_lanes: impl IntoIterator<Item = SyncMaterialLane>,
        include_evidence_refs: bool,
    ) -> Self {
        Self {
            shape_id: shape_id.into(),
            material_lanes: material_lanes.into_iter().collect(),
            include_evidence_refs,
        }
    }

    #[must_use]
    pub fn metadata_only() -> Self {
        Self::new("shape.metadata_only", [SyncMaterialLane::Metadata], false)
    }

    #[must_use]
    pub fn metadata_with_evidence_refs() -> Self {
        Self::new(
            "shape.metadata_with_evidence_refs",
            [SyncMaterialLane::Metadata, SyncMaterialLane::EvidenceRef],
            true,
        )
    }

    #[must_use]
    pub fn body_without_embeddings() -> Self {
        Self::new(
            "shape.body_without_embeddings",
            [
                SyncMaterialLane::Metadata,
                SyncMaterialLane::Body,
                SyncMaterialLane::EvidenceRef,
            ],
            true,
        )
    }
}

/// Export profile assigned to one or more peers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectiveSyncProfile {
    pub schema: String,
    pub profile_id: String,
    pub description: String,
    pub shape: SelectiveSyncShape,
    pub allowed_levels: BTreeSet<String>,
    pub denied_levels: BTreeSet<String>,
    pub allowed_kinds: BTreeSet<String>,
    pub denied_kinds: BTreeSet<String>,
    pub allowed_tags: BTreeSet<String>,
    pub required_tags: BTreeSet<String>,
    pub denied_tags: BTreeSet<String>,
    pub allowed_trust_classes: BTreeSet<SyncTrustClass>,
    pub denied_material_lanes: BTreeSet<SyncMaterialLane>,
    pub created_after_secs: Option<i64>,
    pub created_before_secs: Option<i64>,
}

impl SelectiveSyncProfile {
    #[must_use]
    pub fn new(
        profile_id: impl Into<String>,
        description: impl Into<String>,
        shape: SelectiveSyncShape,
    ) -> Self {
        Self {
            schema: SELECTIVE_SYNC_PROFILE_SCHEMA_V1.to_owned(),
            profile_id: profile_id.into(),
            description: description.into(),
            shape,
            allowed_levels: BTreeSet::new(),
            denied_levels: BTreeSet::new(),
            allowed_kinds: BTreeSet::new(),
            denied_kinds: BTreeSet::new(),
            allowed_tags: BTreeSet::new(),
            required_tags: BTreeSet::new(),
            denied_tags: BTreeSet::new(),
            allowed_trust_classes: BTreeSet::new(),
            denied_material_lanes: BTreeSet::new(),
            created_after_secs: None,
            created_before_secs: None,
        }
    }

    #[must_use]
    pub fn with_denied_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.denied_tags.extend(tags.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn with_allowed_levels(
        mut self,
        levels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_levels
            .extend(levels.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn with_allowed_kinds(
        mut self,
        kinds: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_kinds.extend(kinds.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn with_allowed_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_tags.extend(tags.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn with_required_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.required_tags.extend(tags.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn with_allowed_trust_classes(
        mut self,
        trust_classes: impl IntoIterator<Item = SyncTrustClass>,
    ) -> Self {
        self.allowed_trust_classes.extend(trust_classes);
        self
    }

    #[must_use]
    pub fn with_time_window(
        mut self,
        created_after_secs: Option<i64>,
        created_before_secs: Option<i64>,
    ) -> Self {
        self.created_after_secs = created_after_secs;
        self.created_before_secs = created_before_secs;
        self
    }
}

/// Safe starter profiles that can be documented and rendered as presets.
#[must_use]
pub fn safe_starter_profiles() -> Vec<SelectiveSyncProfile> {
    vec![
        SelectiveSyncProfile::new(
            STARTER_PROFILE_METADATA_ONLY,
            "Share metadata lanes only; bodies, embeddings, and evidence refs stay local.",
            SelectiveSyncShape::metadata_only(),
        )
        .with_denied_tags(DEFAULT_DENIED_TAGS.iter().copied())
        .with_allowed_trust_classes(SyncTrustClass::all()),
        SelectiveSyncProfile::new(
            STARTER_PROFILE_EVIDENCE_REFS,
            "Share metadata plus evidence references, without body or embedding payloads.",
            SelectiveSyncShape::metadata_with_evidence_refs(),
        )
        .with_denied_tags(DEFAULT_DENIED_TAGS.iter().copied())
        .with_allowed_trust_classes(SyncTrustClass::all()),
        SelectiveSyncProfile::new(
            STARTER_PROFILE_TRUSTED_BODIES,
            "Share trusted body text and evidence references; embeddings remain local-only.",
            SelectiveSyncShape::body_without_embeddings(),
        )
        .with_denied_tags(DEFAULT_DENIED_TAGS.iter().copied())
        .with_allowed_trust_classes(SyncTrustClass::reviewed_or_validated()),
    ]
}

/// Serializable config representation for named profiles and per-peer
/// subscriptions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectiveSyncConfig {
    pub schema: String,
    pub profiles: Vec<SelectiveSyncProfile>,
    pub subscriptions: Vec<PeerSyncSubscription>,
}

impl SelectiveSyncConfig {
    #[must_use]
    pub fn new(
        profiles: impl IntoIterator<Item = SelectiveSyncProfile>,
        subscriptions: impl IntoIterator<Item = PeerSyncSubscription>,
    ) -> Self {
        let mut profiles = profiles.into_iter().collect::<Vec<_>>();
        profiles.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
        let mut subscriptions = subscriptions.into_iter().collect::<Vec<_>>();
        subscriptions.sort_by(|left, right| {
            left.peer_id
                .cmp(&right.peer_id)
                .then_with(|| left.profile_id.cmp(&right.profile_id))
        });
        Self {
            schema: SELECTIVE_SYNC_CONFIG_SCHEMA_V1.to_owned(),
            profiles,
            subscriptions,
        }
    }

    #[must_use]
    pub fn safe_starter_config() -> Self {
        Self::new(
            safe_starter_profiles(),
            std::iter::empty::<PeerSyncSubscription>(),
        )
    }

    #[must_use]
    pub fn with_subscriptions(
        mut self,
        subscriptions: impl IntoIterator<Item = PeerSyncSubscription>,
    ) -> Self {
        self.subscriptions.extend(subscriptions);
        self.subscriptions.sort_by(|left, right| {
            left.peer_id
                .cmp(&right.peer_id)
                .then_with(|| left.profile_id.cmp(&right.profile_id))
        });
        self
    }

    #[must_use]
    pub fn profile(&self, profile_id: &str) -> Option<&SelectiveSyncProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.profile_id == profile_id)
    }

    #[must_use]
    pub fn summary(&self) -> SelectiveSyncStatusSummary {
        let starter_profile_ids = self
            .profiles
            .iter()
            .map(|profile| profile.profile_id.clone())
            .collect::<Vec<_>>();
        let subscription_profile_ids = self
            .subscriptions
            .iter()
            .map(|subscription| subscription.profile_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let default_profile_id = if starter_profile_ids
            .iter()
            .any(|profile_id| profile_id == STARTER_PROFILE_METADATA_ONLY)
        {
            STARTER_PROFILE_METADATA_ONLY.to_owned()
        } else {
            starter_profile_ids
                .first()
                .cloned()
                .unwrap_or_else(|| STARTER_PROFILE_METADATA_ONLY.to_owned())
        };
        let body_lanes_default_allowed = self.profile(&default_profile_id).map_or(false, |p| {
            p.shape.material_lanes.contains(&SyncMaterialLane::Body)
        });
        let embedding_lanes_allowed = self.profiles.iter().any(|p| {
            p.shape
                .material_lanes
                .contains(&SyncMaterialLane::Embedding)
        });
        SelectiveSyncStatusSummary {
            schema: SELECTIVE_SYNC_STATUS_SCHEMA_V1.to_owned(),
            default_profile_id,
            profile_count: self.profiles.len(),
            subscription_count: self.subscriptions.len(),
            starter_profile_ids,
            subscription_profile_ids,
            body_lanes_default_allowed,
            embedding_lanes_allowed,
        }
    }

    #[must_use]
    pub fn preview_for_subscription(
        &self,
        subscription: &PeerSyncSubscription,
        candidates: &[SelectiveSyncCandidate],
    ) -> SelectiveSyncPreview {
        let profile = self.profile(&subscription.profile_id).cloned().unwrap_or_else(|| {
            SelectiveSyncProfile::new(
                "__missing_profile__",
                "Missing profile placeholder; every candidate denies with profile_not_subscribed.",
                SelectiveSyncShape::metadata_only(),
            )
        });
        build_selective_sync_preview(SelectiveSyncPreviewInput {
            subscription: subscription.clone(),
            profile,
            candidates: candidates.to_vec(),
        })
    }

    #[must_use]
    pub fn previews_for_candidates(
        &self,
        candidates: &[SelectiveSyncCandidate],
    ) -> Vec<SelectiveSyncPreview> {
        self.subscriptions
            .iter()
            .map(|subscription| self.preview_for_subscription(subscription, candidates))
            .collect()
    }
}

/// Compact status block embedded in foreground mesh status reports.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectiveSyncStatusSummary {
    pub schema: String,
    pub default_profile_id: String,
    pub profile_count: usize,
    pub subscription_count: usize,
    pub starter_profile_ids: Vec<String>,
    pub subscription_profile_ids: Vec<String>,
    pub body_lanes_default_allowed: bool,
    pub embedding_lanes_allowed: bool,
}

/// Per-peer subscription binding. The subscription chooses the profile and can
/// further constrain which origin workspaces the peer receives.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerSyncSubscription {
    pub peer_id: String,
    pub profile_id: String,
    pub enabled: bool,
    pub allowed_origin_workspace_ids: BTreeSet<String>,
    pub denied_origin_workspace_ids: BTreeSet<String>,
}

impl PeerSyncSubscription {
    #[must_use]
    pub fn new(peer_id: impl Into<String>, profile_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            profile_id: profile_id.into(),
            enabled: true,
            allowed_origin_workspace_ids: BTreeSet::new(),
            denied_origin_workspace_ids: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    #[must_use]
    pub fn with_allowed_origins(
        mut self,
        origins: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_origin_workspace_ids
            .extend(origins.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn with_denied_origins(
        mut self,
        origins: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.denied_origin_workspace_ids
            .extend(origins.into_iter().map(Into::into));
        self
    }
}

/// One materialized export candidate before peer-specific filtering.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectiveSyncCandidate {
    pub memory_id: String,
    pub origin_workspace_id: String,
    pub level: String,
    pub kind: String,
    pub tags: BTreeSet<String>,
    pub trust_class: SyncTrustClass,
    pub material_lane: SyncMaterialLane,
    pub created_at_secs: i64,
    pub has_evidence_refs: bool,
    pub estimated_bytes: u64,
    pub withdrawn: bool,
    pub tombstoned: bool,
}

impl SelectiveSyncCandidate {
    #[must_use]
    pub fn new(
        memory_id: impl Into<String>,
        origin_workspace_id: impl Into<String>,
        level: impl Into<String>,
        kind: impl Into<String>,
        material_lane: SyncMaterialLane,
    ) -> Self {
        Self {
            memory_id: memory_id.into(),
            origin_workspace_id: origin_workspace_id.into(),
            level: level.into(),
            kind: kind.into(),
            tags: BTreeSet::new(),
            trust_class: SyncTrustClass::AgentProposed,
            material_lane,
            created_at_secs: 0,
            has_evidence_refs: false,
            estimated_bytes: 0,
            withdrawn: false,
            tombstoned: false,
        }
    }

    #[must_use]
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags.extend(tags.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn with_trust_class(mut self, trust_class: SyncTrustClass) -> Self {
        self.trust_class = trust_class;
        self
    }

    #[must_use]
    pub fn with_created_at_secs(mut self, created_at_secs: i64) -> Self {
        self.created_at_secs = created_at_secs;
        self
    }

    #[must_use]
    pub fn with_evidence_refs(mut self, has_evidence_refs: bool) -> Self {
        self.has_evidence_refs = has_evidence_refs;
        self
    }

    #[must_use]
    pub fn with_estimated_bytes(mut self, estimated_bytes: u64) -> Self {
        self.estimated_bytes = estimated_bytes;
        self
    }

    #[must_use]
    pub fn with_withdrawn(mut self, withdrawn: bool) -> Self {
        self.withdrawn = withdrawn;
        self
    }

    #[must_use]
    pub fn with_tombstoned(mut self, tombstoned: bool) -> Self {
        self.tombstoned = tombstoned;
        self
    }
}

/// Deterministic allow/deny outcome.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncFilterDecision {
    Allow,
    Deny,
}

impl SyncFilterDecision {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

/// First failing rule for a denied candidate. Order is the exported deny
/// precedence: subscription gates, explicit denies, allow filters, then shape
/// constraints.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDenyReason {
    SubscriptionDisabled,
    ProfileNotSubscribed,
    OriginWorkspaceDenied,
    OriginWorkspaceNotAllowed,
    Withdrawn,
    Tombstoned,
    LevelDenied,
    KindDenied,
    TagDenied,
    LevelNotAllowed,
    KindNotAllowed,
    RequiredTagMissing,
    TagNotAllowed,
    TrustClassNotAllowed,
    CreatedBeforeWindow,
    CreatedAfterWindow,
    MaterialLaneDenied,
    EvidenceRefsExcluded,
    MaterialLaneNotAllowed,
}

impl SyncDenyReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SubscriptionDisabled => "subscription_disabled",
            Self::ProfileNotSubscribed => "profile_not_subscribed",
            Self::OriginWorkspaceDenied => "origin_workspace_denied",
            Self::OriginWorkspaceNotAllowed => "origin_workspace_not_allowed",
            Self::Withdrawn => "withdrawn",
            Self::Tombstoned => "tombstoned",
            Self::LevelDenied => "level_denied",
            Self::KindDenied => "kind_denied",
            Self::TagDenied => "tag_denied",
            Self::LevelNotAllowed => "level_not_allowed",
            Self::KindNotAllowed => "kind_not_allowed",
            Self::RequiredTagMissing => "required_tag_missing",
            Self::TagNotAllowed => "tag_not_allowed",
            Self::TrustClassNotAllowed => "trust_class_not_allowed",
            Self::CreatedBeforeWindow => "created_before_window",
            Self::CreatedAfterWindow => "created_after_window",
            Self::MaterialLaneDenied => "material_lane_denied",
            Self::EvidenceRefsExcluded => "evidence_refs_excluded",
            Self::MaterialLaneNotAllowed => "material_lane_not_allowed",
        }
    }
}

/// Row emitted for preview, status, or structured sync logs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectiveSyncDecisionRow {
    pub profile_id: String,
    pub peer_id: String,
    pub memory_id: String,
    pub origin_workspace_id: String,
    pub material_lane: SyncMaterialLane,
    pub level: String,
    pub kind: String,
    pub trust_class: SyncTrustClass,
    pub decision: SyncFilterDecision,
    pub deny_reason: Option<SyncDenyReason>,
    pub explanation: String,
    pub estimated_bytes: u64,
}

/// Preview/report input for one peer subscription.
#[derive(Clone, Debug)]
pub struct SelectiveSyncPreviewInput {
    pub subscription: PeerSyncSubscription,
    pub profile: SelectiveSyncProfile,
    pub candidates: Vec<SelectiveSyncCandidate>,
}

/// Deterministic dry-run preview for one peer/profile pair.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectiveSyncPreview {
    pub schema: String,
    pub profile_id: String,
    pub peer_id: String,
    pub candidate_count: usize,
    pub allowed_count: usize,
    pub denied_count: usize,
    pub allowed_bytes: u64,
    pub denied_bytes: u64,
    pub denied_by_reason: BTreeMap<String, usize>,
    pub allowed_by_material_lane: BTreeMap<String, usize>,
    pub rows: Vec<SelectiveSyncDecisionRow>,
}

/// Build a deterministic preview and per-candidate decision log.
#[must_use]
pub fn build_selective_sync_preview(input: SelectiveSyncPreviewInput) -> SelectiveSyncPreview {
    let mut rows = input
        .candidates
        .into_iter()
        .map(|candidate| decision_row(&input.subscription, &input.profile, candidate))
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| {
        left.memory_id
            .cmp(&right.memory_id)
            .then_with(|| left.material_lane.cmp(&right.material_lane))
            .then_with(|| left.origin_workspace_id.cmp(&right.origin_workspace_id))
            .then_with(|| left.level.cmp(&right.level))
            .then_with(|| left.kind.cmp(&right.kind))
    });

    let mut allowed_count = 0usize;
    let mut denied_count = 0usize;
    let mut allowed_bytes = 0u64;
    let mut denied_bytes = 0u64;
    let mut denied_by_reason = BTreeMap::new();
    let mut allowed_by_material_lane = BTreeMap::new();

    for row in &rows {
        match row.decision {
            SyncFilterDecision::Allow => {
                allowed_count += 1;
                allowed_bytes = allowed_bytes.saturating_add(row.estimated_bytes);
                *allowed_by_material_lane
                    .entry(row.material_lane.as_str().to_owned())
                    .or_insert(0) += 1;
            }
            SyncFilterDecision::Deny => {
                denied_count += 1;
                denied_bytes = denied_bytes.saturating_add(row.estimated_bytes);
                if let Some(reason) = row.deny_reason {
                    *denied_by_reason
                        .entry(reason.as_str().to_owned())
                        .or_insert(0) += 1;
                }
            }
        }
    }

    SelectiveSyncPreview {
        schema: SELECTIVE_SYNC_PREVIEW_SCHEMA_V1.to_owned(),
        profile_id: input.profile.profile_id,
        peer_id: input.subscription.peer_id,
        candidate_count: rows.len(),
        allowed_count,
        denied_count,
        allowed_bytes,
        denied_bytes,
        denied_by_reason,
        allowed_by_material_lane,
        rows,
    }
}

fn decision_row(
    subscription: &PeerSyncSubscription,
    profile: &SelectiveSyncProfile,
    candidate: SelectiveSyncCandidate,
) -> SelectiveSyncDecisionRow {
    let deny_reason = deny_reason(subscription, profile, &candidate);
    let explanation = decision_explanation(profile, candidate.material_lane, deny_reason);
    SelectiveSyncDecisionRow {
        profile_id: profile.profile_id.clone(),
        peer_id: subscription.peer_id.clone(),
        memory_id: candidate.memory_id,
        origin_workspace_id: candidate.origin_workspace_id,
        material_lane: candidate.material_lane,
        level: candidate.level,
        kind: candidate.kind,
        trust_class: candidate.trust_class,
        decision: if deny_reason.is_some() {
            SyncFilterDecision::Deny
        } else {
            SyncFilterDecision::Allow
        },
        deny_reason,
        explanation,
        estimated_bytes: candidate.estimated_bytes,
    }
}

fn decision_explanation(
    profile: &SelectiveSyncProfile,
    material_lane: SyncMaterialLane,
    deny_reason: Option<SyncDenyReason>,
) -> String {
    match deny_reason {
        Some(reason) => format!(
            "denied by profile {}: {}",
            profile.profile_id,
            reason.as_str()
        ),
        None => format!(
            "allowed by profile {} for {} lane",
            profile.profile_id,
            material_lane.as_str()
        ),
    }
}

fn deny_reason(
    subscription: &PeerSyncSubscription,
    profile: &SelectiveSyncProfile,
    candidate: &SelectiveSyncCandidate,
) -> Option<SyncDenyReason> {
    if !subscription.enabled {
        return Some(SyncDenyReason::SubscriptionDisabled);
    }
    if subscription.profile_id != profile.profile_id {
        return Some(SyncDenyReason::ProfileNotSubscribed);
    }
    if subscription
        .denied_origin_workspace_ids
        .contains(&candidate.origin_workspace_id)
    {
        return Some(SyncDenyReason::OriginWorkspaceDenied);
    }
    if !subscription.allowed_origin_workspace_ids.is_empty()
        && !subscription
            .allowed_origin_workspace_ids
            .contains(&candidate.origin_workspace_id)
    {
        return Some(SyncDenyReason::OriginWorkspaceNotAllowed);
    }
    if candidate.withdrawn {
        return Some(SyncDenyReason::Withdrawn);
    }
    if candidate.tombstoned {
        return Some(SyncDenyReason::Tombstoned);
    }
    if profile.denied_levels.contains(&candidate.level) {
        return Some(SyncDenyReason::LevelDenied);
    }
    if profile.denied_kinds.contains(&candidate.kind) {
        return Some(SyncDenyReason::KindDenied);
    }
    if candidate
        .tags
        .iter()
        .any(|tag| profile.denied_tags.contains(tag))
    {
        return Some(SyncDenyReason::TagDenied);
    }
    if !profile.allowed_levels.is_empty() && !profile.allowed_levels.contains(&candidate.level) {
        return Some(SyncDenyReason::LevelNotAllowed);
    }
    if !profile.allowed_kinds.is_empty() && !profile.allowed_kinds.contains(&candidate.kind) {
        return Some(SyncDenyReason::KindNotAllowed);
    }
    if profile
        .required_tags
        .iter()
        .any(|tag| !candidate.tags.contains(tag))
    {
        return Some(SyncDenyReason::RequiredTagMissing);
    }
    if !profile.allowed_tags.is_empty()
        && !candidate
            .tags
            .iter()
            .any(|tag| profile.allowed_tags.contains(tag))
    {
        return Some(SyncDenyReason::TagNotAllowed);
    }
    if !profile.allowed_trust_classes.is_empty()
        && !profile
            .allowed_trust_classes
            .contains(&candidate.trust_class)
    {
        return Some(SyncDenyReason::TrustClassNotAllowed);
    }
    if let Some(created_after_secs) = profile.created_after_secs
        && candidate.created_at_secs < created_after_secs
    {
        return Some(SyncDenyReason::CreatedBeforeWindow);
    }
    if let Some(created_before_secs) = profile.created_before_secs
        && candidate.created_at_secs > created_before_secs
    {
        return Some(SyncDenyReason::CreatedAfterWindow);
    }
    if profile
        .denied_material_lanes
        .contains(&candidate.material_lane)
    {
        return Some(SyncDenyReason::MaterialLaneDenied);
    }
    if candidate.material_lane == SyncMaterialLane::EvidenceRef
        && (!profile.shape.include_evidence_refs || !candidate.has_evidence_refs)
    {
        return Some(SyncDenyReason::EvidenceRefsExcluded);
    }
    if !profile
        .shape
        .material_lanes
        .contains(&candidate.material_lane)
    {
        return Some(SyncDenyReason::MaterialLaneNotAllowed);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_trust_class_sets_include_peer_human_attested() {
        assert_eq!(
            SyncTrustClass::PeerHumanAttested.as_str(),
            "peer_human_attested"
        );

        let all = SyncTrustClass::all();
        assert_eq!(all.len(), 6);
        assert!(all.contains(&SyncTrustClass::PeerHumanAttested));
        assert_eq!(
            all.iter()
                .copied()
                .map(SyncTrustClass::as_str)
                .collect::<Vec<_>>(),
            vec![
                "human_explicit",
                "human_revised",
                "peer_human_attested",
                "agent_validated",
                "agent_proposed",
                "external",
            ]
        );

        let reviewed_or_validated = SyncTrustClass::reviewed_or_validated();
        assert!(reviewed_or_validated.contains(&SyncTrustClass::PeerHumanAttested));
        assert!(!reviewed_or_validated.contains(&SyncTrustClass::HumanExplicit));
    }

    fn subscription(profile_id: &str) -> PeerSyncSubscription {
        PeerSyncSubscription::new("peer-a", profile_id)
    }

    fn profile(shape: SelectiveSyncShape) -> SelectiveSyncProfile {
        SelectiveSyncProfile::new("profile-a", "test profile", shape)
    }

    fn candidate(memory_id: &str, lane: SyncMaterialLane) -> SelectiveSyncCandidate {
        SelectiveSyncCandidate::new(memory_id, "workspace-a", "procedural", "rule", lane)
            .with_trust_class(SyncTrustClass::AgentValidated)
            .with_estimated_bytes(10)
    }

    #[test]
    fn denied_tags_win_before_allowed_material_lanes() {
        let preview = build_selective_sync_preview(SelectiveSyncPreviewInput {
            subscription: subscription("profile-a"),
            profile: profile(SelectiveSyncShape::body_without_embeddings())
                .with_allowed_tags(["public"])
                .with_denied_tags(["secret"]),
            candidates: vec![
                candidate("mem-1", SyncMaterialLane::Body).with_tags(["public", "secret"]),
            ],
        });

        assert_eq!(preview.candidate_count, 1);
        assert_eq!(preview.allowed_count, 0);
        assert_eq!(preview.denied_count, 1);
        assert_eq!(preview.rows[0].deny_reason, Some(SyncDenyReason::TagDenied));
        assert_eq!(preview.denied_by_reason.get("tag_denied"), Some(&1));
    }

    #[test]
    fn withdrawn_lifecycle_blocks_export_before_content_filters() {
        let preview = build_selective_sync_preview(SelectiveSyncPreviewInput {
            subscription: subscription("profile-a"),
            profile: profile(SelectiveSyncShape::body_without_embeddings())
                .with_allowed_tags(["public"])
                .with_denied_tags(["secret"]),
            candidates: vec![
                candidate("mem-withdrawn", SyncMaterialLane::Body)
                    .with_withdrawn(true)
                    .with_tags(["public", "secret"]),
            ],
        });

        assert_eq!(preview.candidate_count, 1);
        assert_eq!(preview.allowed_count, 0);
        assert_eq!(preview.denied_count, 1);
        assert_eq!(preview.rows[0].deny_reason, Some(SyncDenyReason::Withdrawn));
        assert_eq!(preview.denied_by_reason.get("withdrawn"), Some(&1));
        assert!(
            preview.rows[0]
                .explanation
                .contains("denied by profile profile-a: withdrawn")
        );
    }

    #[test]
    fn profile_assignment_splits_two_peers_from_same_candidates() {
        let candidates = vec![
            candidate("mem-1", SyncMaterialLane::Metadata),
            candidate("mem-1", SyncMaterialLane::Body),
            candidate("mem-1", SyncMaterialLane::Embedding),
        ];

        let metadata_preview = build_selective_sync_preview(SelectiveSyncPreviewInput {
            subscription: PeerSyncSubscription::new("peer-meta", STARTER_PROFILE_METADATA_ONLY),
            profile: safe_starter_profiles()
                .into_iter()
                .find(|profile| profile.profile_id == STARTER_PROFILE_METADATA_ONLY)
                .expect("starter profile exists"),
            candidates: candidates.clone(),
        });
        let body_preview = build_selective_sync_preview(SelectiveSyncPreviewInput {
            subscription: PeerSyncSubscription::new("peer-body", STARTER_PROFILE_TRUSTED_BODIES),
            profile: safe_starter_profiles()
                .into_iter()
                .find(|profile| profile.profile_id == STARTER_PROFILE_TRUSTED_BODIES)
                .expect("starter profile exists"),
            candidates,
        });

        assert_eq!(metadata_preview.peer_id, "peer-meta");
        assert_eq!(metadata_preview.allowed_count, 1);
        assert_eq!(metadata_preview.denied_count, 2);
        assert_eq!(body_preview.peer_id, "peer-body");
        assert_eq!(body_preview.allowed_count, 2);
        assert_eq!(body_preview.denied_count, 1);
        assert_eq!(
            body_preview
                .denied_by_reason
                .get("material_lane_not_allowed"),
            Some(&1)
        );
    }

    #[test]
    fn preview_rows_are_sorted_for_deterministic_logs() {
        let preview = build_selective_sync_preview(SelectiveSyncPreviewInput {
            subscription: subscription("profile-a"),
            profile: profile(SelectiveSyncShape::body_without_embeddings()),
            candidates: vec![
                candidate("mem-b", SyncMaterialLane::Body),
                candidate("mem-a", SyncMaterialLane::EvidenceRef).with_evidence_refs(true),
                candidate("mem-a", SyncMaterialLane::Metadata),
            ],
        });

        let keys = preview
            .rows
            .iter()
            .map(|row| (row.memory_id.as_str(), row.material_lane.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                ("mem-a", "metadata"),
                ("mem-a", "evidence_ref"),
                ("mem-b", "body")
            ]
        );
    }

    #[test]
    fn subscriptions_and_time_windows_are_deny_reasons() {
        let strict_profile = profile(SelectiveSyncShape::metadata_with_evidence_refs())
            .with_allowed_trust_classes([SyncTrustClass::HumanRevised])
            .with_time_window(Some(100), Some(200));

        let disabled_preview = build_selective_sync_preview(SelectiveSyncPreviewInput {
            subscription: subscription("profile-a").disabled(),
            profile: strict_profile.clone(),
            candidates: vec![candidate("mem-1", SyncMaterialLane::Metadata)],
        });
        assert_eq!(
            disabled_preview.rows[0].deny_reason,
            Some(SyncDenyReason::SubscriptionDisabled)
        );

        let preview = build_selective_sync_preview(SelectiveSyncPreviewInput {
            subscription: subscription("profile-a").with_allowed_origins(["workspace-a"]),
            profile: strict_profile,
            candidates: vec![
                candidate("mem-2", SyncMaterialLane::Metadata)
                    .with_trust_class(SyncTrustClass::External)
                    .with_created_at_secs(150),
                candidate("mem-3", SyncMaterialLane::Metadata)
                    .with_trust_class(SyncTrustClass::HumanRevised)
                    .with_created_at_secs(50),
                candidate("mem-4", SyncMaterialLane::EvidenceRef)
                    .with_trust_class(SyncTrustClass::HumanRevised)
                    .with_created_at_secs(150),
            ],
        });

        assert_eq!(
            preview.rows[0].deny_reason,
            Some(SyncDenyReason::TrustClassNotAllowed)
        );
        assert_eq!(
            preview.rows[1].deny_reason,
            Some(SyncDenyReason::CreatedBeforeWindow)
        );
        assert_eq!(
            preview.rows[2].deny_reason,
            Some(SyncDenyReason::EvidenceRefsExcluded)
        );
    }

    #[test]
    fn config_previews_split_two_peers_with_structured_counts() {
        let candidates = vec![
            candidate("mem-a", SyncMaterialLane::Metadata),
            candidate("mem-a", SyncMaterialLane::Body),
            candidate("mem-a", SyncMaterialLane::Embedding),
            candidate("mem-b", SyncMaterialLane::EvidenceRef).with_evidence_refs(true),
        ];
        let config = SelectiveSyncConfig::safe_starter_config().with_subscriptions([
            PeerSyncSubscription::new("peer-a-metadata", STARTER_PROFILE_METADATA_ONLY),
            PeerSyncSubscription::new("peer-b-body", STARTER_PROFILE_TRUSTED_BODIES),
        ]);

        let previews = config.previews_for_candidates(&candidates);
        assert_eq!(previews.len(), 2);

        let metadata_preview = &previews[0];
        assert_eq!(metadata_preview.profile_id, STARTER_PROFILE_METADATA_ONLY);
        assert_eq!(metadata_preview.peer_id, "peer-a-metadata");
        assert_eq!(metadata_preview.candidate_count, 4);
        assert_eq!(metadata_preview.allowed_count, 1);
        assert_eq!(metadata_preview.denied_count, 3);
        assert_eq!(
            metadata_preview
                .denied_by_reason
                .get("material_lane_not_allowed"),
            Some(&2)
        );
        assert_eq!(
            metadata_preview
                .denied_by_reason
                .get("evidence_refs_excluded"),
            Some(&1)
        );
        assert!(
            metadata_preview.rows[0]
                .explanation
                .contains("allowed by profile starter.metadata_only")
        );

        let body_preview = &previews[1];
        assert_eq!(body_preview.profile_id, STARTER_PROFILE_TRUSTED_BODIES);
        assert_eq!(body_preview.peer_id, "peer-b-body");
        assert_eq!(body_preview.candidate_count, 4);
        assert_eq!(body_preview.allowed_count, 3);
        assert_eq!(body_preview.denied_count, 1);
        assert_eq!(
            body_preview
                .denied_by_reason
                .get("material_lane_not_allowed"),
            Some(&1)
        );

        let summary = config.summary();
        assert_eq!(summary.profile_count, 3);
        assert_eq!(summary.subscription_count, 2);
        assert!(!summary.body_lanes_default_allowed);
        assert!(!summary.embedding_lanes_allowed);
    }
}
