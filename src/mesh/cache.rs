//! SRR6.26 mesh cache retention, quota, eviction, and body lifecycle model.
//!
//! This module is deliberately pure policy logic. Mesh cache rows and fetched
//! bodies are derived peer material; eviction decisions may remove those
//! derived artifacts, but they must never target local source-of-truth
//! memories. Repository and CLI adapters can wire this model into
//! `mesh_body_cache_metadata` without changing the boundary rules.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

pub const MESH_CACHE_RETENTION_SCHEMA_V1: &str = "ee.mesh.cache_retention.v1";
pub const MESH_WITHDRAWAL_PURGE_SCHEMA_V1: &str = "ee.mesh.withdrawal_purge.v1";
pub const MESH_CACHE_EVICT_AUDIT_ACTION: &str = "mesh.cache.evict";
pub const MESH_CACHE_PURGE_AUDIT_ACTION: &str = "mesh.cache.purge";
pub const WITHDRAWAL_RESIDUAL_METADATA_REASON: &str =
    "metadata_tombstone_preserves_withdrawal_provenance";
pub const WITHDRAWAL_UNAVAILABLE_PEER_REPLAY_REASON: &str =
    "peer_unreachable_withdrawal_replay_required";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum MeshCacheLane {
    Metadata,
    Body,
    Embedding,
}

impl MeshCacheLane {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Body => "body",
            Self::Embedding => "embedding",
        }
    }

    const fn eviction_rank(self) -> u8 {
        match self {
            Self::Body => 0,
            Self::Embedding => 1,
            Self::Metadata => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MeshCacheStatus {
    MetadataOnly,
    Available,
    Quarantined,
    Evicted,
    Expired,
}

impl MeshCacheStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MetadataOnly => "metadata_only",
            Self::Available => "available",
            Self::Quarantined => "quarantined",
            Self::Evicted => "evicted",
            Self::Expired => "expired",
        }
    }

    const fn has_cache_bytes(self) -> bool {
        !matches!(self, Self::Evicted)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MeshCacheBoundary {
    DerivedPeerCache,
    LocalSourceTruth,
}

impl MeshCacheBoundary {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DerivedPeerCache => "derived_peer_cache",
            Self::LocalSourceTruth => "local_source_truth",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshCacheEntry {
    pub cache_key: String,
    pub peer_id: String,
    pub origin_workspace_id: String,
    pub logical_memory_id: String,
    pub lane: MeshCacheLane,
    pub status: MeshCacheStatus,
    pub boundary: MeshCacheBoundary,
    pub bytes: u64,
    pub last_access_seq: u64,
    pub retention_score: u16,
    pub content_hash: Option<String>,
    pub expires_at_epoch_ms: Option<u64>,
}

impl MeshCacheEntry {
    #[must_use]
    pub fn derived(
        cache_key: impl Into<String>,
        peer_id: impl Into<String>,
        lane: MeshCacheLane,
        bytes: u64,
    ) -> Self {
        let cache_key = cache_key.into();
        let peer_id = peer_id.into();
        Self {
            origin_workspace_id: format!("wsp_{peer_id}"),
            logical_memory_id: format!("mem_{cache_key}"),
            cache_key,
            peer_id,
            lane,
            status: MeshCacheStatus::Available,
            boundary: MeshCacheBoundary::DerivedPeerCache,
            bytes,
            last_access_seq: 0,
            retention_score: 500,
            content_hash: None,
            expires_at_epoch_ms: None,
        }
    }

    #[must_use]
    pub fn local_source_truth(
        cache_key: impl Into<String>,
        lane: MeshCacheLane,
        bytes: u64,
    ) -> Self {
        let cache_key = cache_key.into();
        Self {
            origin_workspace_id: "wsp_local".to_owned(),
            logical_memory_id: format!("mem_{cache_key}"),
            cache_key,
            peer_id: "local".to_owned(),
            lane,
            status: MeshCacheStatus::Available,
            boundary: MeshCacheBoundary::LocalSourceTruth,
            bytes,
            last_access_seq: 0,
            retention_score: u16::MAX,
            content_hash: None,
            expires_at_epoch_ms: None,
        }
    }

    #[must_use]
    pub fn with_status(mut self, status: MeshCacheStatus) -> Self {
        self.status = status;
        self
    }

    #[must_use]
    pub fn with_last_access_seq(mut self, last_access_seq: u64) -> Self {
        self.last_access_seq = last_access_seq;
        self
    }

    #[must_use]
    pub fn with_retention_score(mut self, retention_score: u16) -> Self {
        self.retention_score = retention_score;
        self
    }

    #[must_use]
    pub fn with_content_hash(mut self, content_hash: impl Into<String>) -> Self {
        self.content_hash = Some(content_hash.into());
        self
    }

    #[must_use]
    pub fn with_origin_workspace_id(mut self, origin_workspace_id: impl Into<String>) -> Self {
        self.origin_workspace_id = origin_workspace_id.into();
        self
    }

    #[must_use]
    pub fn with_logical_memory_id(mut self, logical_memory_id: impl Into<String>) -> Self {
        self.logical_memory_id = logical_memory_id.into();
        self
    }

    #[must_use]
    pub fn with_expires_at_epoch_ms(mut self, expires_at_epoch_ms: u64) -> Self {
        self.expires_at_epoch_ms = Some(expires_at_epoch_ms);
        self
    }

    #[must_use]
    pub fn is_derived_cache(&self) -> bool {
        self.boundary == MeshCacheBoundary::DerivedPeerCache
    }

    #[must_use]
    pub fn is_billable_cache(&self) -> bool {
        self.is_derived_cache() && self.status.has_cache_bytes()
    }

    #[must_use]
    pub fn is_expired_at(&self, now_epoch_ms: u64) -> bool {
        self.status == MeshCacheStatus::Expired
            || self
                .expires_at_epoch_ms
                .is_some_and(|expires_at| expires_at <= now_epoch_ms)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MeshCacheQuotas {
    pub global_bytes: Option<u64>,
    pub per_peer_bytes: Option<u64>,
    pub metadata_bytes: Option<u64>,
    pub body_bytes: Option<u64>,
    pub embedding_bytes: Option<u64>,
}

impl MeshCacheQuotas {
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            global_bytes: None,
            per_peer_bytes: None,
            metadata_bytes: None,
            body_bytes: None,
            embedding_bytes: None,
        }
    }

    const fn lane_limit(self, lane: MeshCacheLane) -> Option<u64> {
        match lane {
            MeshCacheLane::Metadata => self.metadata_bytes,
            MeshCacheLane::Body => self.body_bytes,
            MeshCacheLane::Embedding => self.embedding_bytes,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshCacheRetentionInput {
    pub entries: Vec<MeshCacheEntry>,
    pub quotas: MeshCacheQuotas,
    pub now_epoch_ms: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MeshCacheUsage {
    pub total_bytes: u64,
    pub metadata_bytes: u64,
    pub body_bytes: u64,
    pub embedding_bytes: u64,
    pub by_peer_bytes: BTreeMap<String, u64>,
    pub entry_count: usize,
}

impl MeshCacheUsage {
    fn add(&mut self, entry: &MeshCacheEntry) {
        if !entry.is_billable_cache() {
            return;
        }

        self.entry_count = self.entry_count.saturating_add(1);
        self.total_bytes = self.total_bytes.saturating_add(entry.bytes);
        match entry.lane {
            MeshCacheLane::Metadata => {
                self.metadata_bytes = self.metadata_bytes.saturating_add(entry.bytes);
            }
            MeshCacheLane::Body => {
                self.body_bytes = self.body_bytes.saturating_add(entry.bytes);
            }
            MeshCacheLane::Embedding => {
                self.embedding_bytes = self.embedding_bytes.saturating_add(entry.bytes);
            }
        }
        let peer_total = self.by_peer_bytes.entry(entry.peer_id.clone()).or_default();
        *peer_total = peer_total.saturating_add(entry.bytes);
    }

    #[must_use]
    pub const fn bytes_for_lane(&self, lane: MeshCacheLane) -> u64 {
        match lane {
            MeshCacheLane::Metadata => self.metadata_bytes,
            MeshCacheLane::Body => self.body_bytes,
            MeshCacheLane::Embedding => self.embedding_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MeshCacheQuotaKind {
    Global,
    Peer,
    Lane,
}

impl MeshCacheQuotaKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Peer => "peer",
            Self::Lane => "lane",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MeshCacheQuotaWarningSeverity {
    NearLimit,
    WouldExceed,
}

impl MeshCacheQuotaWarningSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NearLimit => "near_limit",
            Self::WouldExceed => "would_exceed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshCacheQuotaWarning {
    pub kind: MeshCacheQuotaKind,
    pub peer_id: Option<String>,
    pub lane: Option<MeshCacheLane>,
    pub bytes_after: u64,
    pub limit_bytes: u64,
    pub severity: MeshCacheQuotaWarningSeverity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MeshCacheEvictionReason {
    Expired,
    PeerQuotaExceeded,
    LaneQuotaExceeded,
    GlobalQuotaExceeded,
    ManualPurge,
}

impl MeshCacheEvictionReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::PeerQuotaExceeded => "peer_quota_exceeded",
            Self::LaneQuotaExceeded => "lane_quota_exceeded",
            Self::GlobalQuotaExceeded => "global_quota_exceeded",
            Self::ManualPurge => "manual_purge",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshCacheEviction {
    pub cache_key: String,
    pub peer_id: String,
    pub lane: MeshCacheLane,
    pub bytes: u64,
    pub status_before: MeshCacheStatus,
    pub status_after: MeshCacheStatus,
    pub reason: MeshCacheEvictionReason,
    pub audit_action: &'static str,
    pub cache_bytes_before: u64,
    pub cache_bytes_after: u64,
    pub evicted_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshCacheRetentionPlan {
    pub schema: &'static str,
    pub usage_before: MeshCacheUsage,
    pub usage_after: MeshCacheUsage,
    pub evictions: Vec<MeshCacheEviction>,
    pub warnings: Vec<MeshCacheQuotaWarning>,
    pub protected_local_source_truth_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshWithdrawalPeerDelivery {
    pub peer_id: String,
    pub reachable: bool,
}

impl MeshWithdrawalPeerDelivery {
    #[must_use]
    pub fn reachable(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            reachable: true,
        }
    }

    #[must_use]
    pub fn unreachable(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            reachable: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshWithdrawalPurgeInput {
    pub entries: Vec<MeshCacheEntry>,
    pub origin_workspace_id: String,
    pub logical_memory_id: String,
    pub peer_deliveries: Vec<MeshWithdrawalPeerDelivery>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MeshWithdrawalLogKind {
    WithdrawalEvent,
    PurgeRequested,
    PurgeApplied,
    PeerUnreachable,
    ResidualMetadataReason,
}

impl MeshWithdrawalLogKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WithdrawalEvent => "withdrawal_event",
            Self::PurgeRequested => "purge_requested",
            Self::PurgeApplied => "purge_applied",
            Self::PeerUnreachable => "peer_unreachable",
            Self::ResidualMetadataReason => "residual_metadata_reason",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshWithdrawalPurgeLog {
    pub kind: MeshWithdrawalLogKind,
    pub peer_id: Option<String>,
    pub cache_key: Option<String>,
    pub lane: Option<MeshCacheLane>,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshWithdrawalResidualMetadata {
    pub cache_key: String,
    pub peer_id: String,
    pub status: MeshCacheStatus,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshWithdrawalReplayTarget {
    pub peer_id: String,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshWithdrawalPurgePlan {
    pub schema: &'static str,
    pub origin_workspace_id: String,
    pub logical_memory_id: String,
    pub cache_bytes_before: u64,
    pub cache_bytes_after: u64,
    pub evictions: Vec<MeshCacheEviction>,
    pub residual_metadata: Vec<MeshWithdrawalResidualMetadata>,
    pub replay_targets: Vec<MeshWithdrawalReplayTarget>,
    pub logs: Vec<MeshWithdrawalPurgeLog>,
    pub protected_local_source_truth_count: usize,
}

impl MeshWithdrawalPurgePlan {
    #[must_use]
    pub fn purged_count(&self) -> usize {
        self.evictions.len()
    }

    #[must_use]
    pub fn residual_metadata_count(&self) -> usize {
        self.residual_metadata.len()
    }
}

impl MeshCacheRetentionPlan {
    #[must_use]
    pub const fn cache_bytes_before(&self) -> u64 {
        self.usage_before.total_bytes
    }

    #[must_use]
    pub const fn cache_bytes_after(&self) -> u64 {
        self.usage_after.total_bytes
    }

    #[must_use]
    pub fn evicted_count(&self) -> usize {
        self.evictions.len()
    }
}

#[must_use]
pub fn plan_mesh_cache_retention(input: &MeshCacheRetentionInput) -> MeshCacheRetentionPlan {
    let mut remaining: BTreeSet<usize> = input
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.is_billable_cache().then_some(index))
        .collect();
    let usage_before = usage_for(&input.entries, &remaining);
    let protected_local_source_truth_count = input
        .entries
        .iter()
        .filter(|entry| entry.boundary == MeshCacheBoundary::LocalSourceTruth)
        .count();
    let mut evictions = Vec::new();

    let mut expired: Vec<usize> = remaining
        .iter()
        .copied()
        .filter(|index| input.entries[*index].is_expired_at(input.now_epoch_ms))
        .collect();
    sort_eviction_candidates(&mut expired, &input.entries);
    for index in expired {
        evict_index(
            index,
            &input.entries,
            &mut remaining,
            &mut evictions,
            MeshCacheEvictionReason::Expired,
        );
    }

    loop {
        let usage = usage_for(&input.entries, &remaining);
        let violations = quota_violations(&usage, &input.quotas);
        if violations.is_empty() {
            let warnings = quota_near_limit_warnings(&usage, &input.quotas, 90);
            return MeshCacheRetentionPlan {
                schema: MESH_CACHE_RETENTION_SCHEMA_V1,
                usage_before,
                usage_after: usage,
                evictions,
                warnings,
                protected_local_source_truth_count,
            };
        }

        let mut candidates: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|index| intersects_quota_violation(&input.entries[*index], &violations))
            .collect();
        if candidates.is_empty() {
            return MeshCacheRetentionPlan {
                schema: MESH_CACHE_RETENTION_SCHEMA_V1,
                usage_before,
                usage_after: usage,
                evictions,
                warnings: violations,
                protected_local_source_truth_count,
            };
        }

        sort_eviction_candidates(&mut candidates, &input.entries);
        let index = candidates[0];
        let reason = reason_for_quota_eviction(&input.entries[index], &violations);
        evict_index(
            index,
            &input.entries,
            &mut remaining,
            &mut evictions,
            reason,
        );
    }
}

#[must_use]
pub fn plan_mesh_withdrawal_cache_purge(
    input: &MeshWithdrawalPurgeInput,
) -> MeshWithdrawalPurgePlan {
    let mut remaining: BTreeSet<usize> = input
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.is_billable_cache().then_some(index))
        .collect();
    let cache_bytes_before = usage_for(&input.entries, &remaining).total_bytes;
    let mut evictions = Vec::new();
    let mut residual_metadata = Vec::new();
    let mut replay_targets = Vec::new();
    let mut logs = vec![
        MeshWithdrawalPurgeLog {
            kind: MeshWithdrawalLogKind::WithdrawalEvent,
            peer_id: None,
            cache_key: None,
            lane: None,
            reason: "share_withdrawal_observed",
        },
        MeshWithdrawalPurgeLog {
            kind: MeshWithdrawalLogKind::PurgeRequested,
            peer_id: None,
            cache_key: None,
            lane: None,
            reason: "best_effort_peer_cache_body_purge",
        },
    ];
    let mut protected_local_source_truth_count = 0;

    let mut candidates: Vec<usize> = input
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| matches_withdrawal(entry, input).then_some(index))
        .collect();
    sort_withdrawal_candidates(&mut candidates, &input.entries);

    for index in candidates {
        let entry = &input.entries[index];
        if entry.boundary == MeshCacheBoundary::LocalSourceTruth {
            protected_local_source_truth_count += 1;
            continue;
        }
        if entry.lane == MeshCacheLane::Metadata {
            residual_metadata.push(MeshWithdrawalResidualMetadata {
                cache_key: entry.cache_key.clone(),
                peer_id: entry.peer_id.clone(),
                status: entry.status,
                reason: WITHDRAWAL_RESIDUAL_METADATA_REASON,
            });
            logs.push(MeshWithdrawalPurgeLog {
                kind: MeshWithdrawalLogKind::ResidualMetadataReason,
                peer_id: Some(entry.peer_id.clone()),
                cache_key: Some(entry.cache_key.clone()),
                lane: Some(entry.lane),
                reason: WITHDRAWAL_RESIDUAL_METADATA_REASON,
            });
            continue;
        }
        if purge_index(index, &input.entries, &mut remaining, &mut evictions) {
            logs.push(MeshWithdrawalPurgeLog {
                kind: MeshWithdrawalLogKind::PurgeApplied,
                peer_id: Some(entry.peer_id.clone()),
                cache_key: Some(entry.cache_key.clone()),
                lane: Some(entry.lane),
                reason: MeshCacheEvictionReason::ManualPurge.as_str(),
            });
        }
    }

    let mut peer_deliveries = input.peer_deliveries.clone();
    peer_deliveries.sort_by(|left, right| left.peer_id.cmp(&right.peer_id));
    for delivery in peer_deliveries {
        if delivery.reachable {
            continue;
        }
        replay_targets.push(MeshWithdrawalReplayTarget {
            peer_id: delivery.peer_id.clone(),
            reason: WITHDRAWAL_UNAVAILABLE_PEER_REPLAY_REASON,
        });
        logs.push(MeshWithdrawalPurgeLog {
            kind: MeshWithdrawalLogKind::PeerUnreachable,
            peer_id: Some(delivery.peer_id),
            cache_key: None,
            lane: None,
            reason: WITHDRAWAL_UNAVAILABLE_PEER_REPLAY_REASON,
        });
    }

    MeshWithdrawalPurgePlan {
        schema: MESH_WITHDRAWAL_PURGE_SCHEMA_V1,
        origin_workspace_id: input.origin_workspace_id.clone(),
        logical_memory_id: input.logical_memory_id.clone(),
        cache_bytes_before,
        cache_bytes_after: usage_for(&input.entries, &remaining).total_bytes,
        evictions,
        residual_metadata,
        replay_targets,
        logs,
        protected_local_source_truth_count,
    }
}

#[must_use]
pub fn eager_replication_warnings(
    existing_entries: &[MeshCacheEntry],
    candidate: &MeshCacheEntry,
    quotas: &MeshCacheQuotas,
    warning_threshold_percent: u8,
) -> Vec<MeshCacheQuotaWarning> {
    let mut usage = usage_for_all(existing_entries);
    usage.add(candidate);
    let mut warnings = quota_violations(&usage, quotas);
    warnings.extend(quota_near_limit_warnings(
        &usage,
        quotas,
        warning_threshold_percent,
    ));
    warnings.sort_by(compare_quota_warning);
    warnings.dedup();
    warnings
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshCacheBodyFetchDecision {
    pub expected_content_hash: String,
    pub actual_local_body_hash: String,
    pub status: MeshCacheStatus,
    pub body_persist_allowed: bool,
    pub quarantine_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MeshCachedBodyLocation {
    MetadataOnly,
    CachedRemoteBody,
    FetchableRemoteBody,
    QuarantinedRemoteBody,
}

impl MeshCachedBodyLocation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MetadataOnly => "metadata_only",
            Self::CachedRemoteBody => "cached_remote_body",
            Self::FetchableRemoteBody => "fetchable_remote_body",
            Self::QuarantinedRemoteBody => "quarantined_remote_body",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshCachedBodyProvenance {
    pub body_cache_key: String,
    pub location: MeshCachedBodyLocation,
    pub content_hash: String,
    pub local_body_hash: Option<String>,
    pub freshness_ref: Option<String>,
    pub reason: &'static str,
}

impl MeshCachedBodyProvenance {
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": "ee.mesh.cached_body_provenance.v1",
            "bodyCacheKey": self.body_cache_key,
            "location": self.location.as_str(),
            "contentHash": self.content_hash,
            "localBodyHash": self.local_body_hash,
            "freshnessRef": self.freshness_ref,
            "reason": self.reason,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshEagerMetadataCacheInput {
    pub body_cache_key: String,
    pub peer_id: String,
    pub origin_workspace_id: String,
    pub logical_memory_id: String,
    pub content_hash: String,
    pub body_ref_json: Option<String>,
    pub preview_hash: Option<String>,
    pub metadata_size_bytes: u64,
    pub advertised_body_bytes: Option<u64>,
    pub policy_allows_metadata_index: bool,
    pub policy_body_fetch_allowed: bool,
    pub local_body_hash: Option<String>,
    pub freshness_ref: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshEagerMetadataCachePlan {
    pub metadata_entry: MeshCacheEntry,
    pub body_cache_key: String,
    pub body_ref_json: Option<String>,
    pub preview_hash: Option<String>,
    pub advertised_body_bytes: Option<u64>,
    pub cache_status: MeshCacheStatus,
    pub local_body_hash: Option<String>,
    pub metadata_index_allowed: bool,
    pub body_fetch_allowed: bool,
    pub provenance: MeshCachedBodyProvenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MeshLazyBodyFetchAction {
    UseCachedBody,
    FetchRemoteBody,
    KeepMetadataOnly,
    QuarantineBody,
}

impl MeshLazyBodyFetchAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UseCachedBody => "use_cached_body",
            Self::FetchRemoteBody => "fetch_remote_body",
            Self::KeepMetadataOnly => "keep_metadata_only",
            Self::QuarantineBody => "quarantine_body",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshLazyBodyFetchInput<'a> {
    pub body_cache_key: &'a str,
    pub expected_content_hash: &'a str,
    pub policy_body_fetch_allowed: bool,
    pub query_requires_body: bool,
    pub remote_body_available: bool,
    pub cached_local_body_hash: Option<&'a str>,
    pub fetched_body: Option<&'a [u8]>,
    pub freshness_ref: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshLazyBodyFetchPlan {
    pub action: MeshLazyBodyFetchAction,
    pub cache_status: MeshCacheStatus,
    pub body_persist_allowed: bool,
    pub actual_local_body_hash: Option<String>,
    pub denial_reason: Option<&'static str>,
    pub degraded_codes: Vec<&'static str>,
    pub provenance: MeshCachedBodyProvenance,
}

#[must_use]
pub fn plan_eager_metadata_cache(
    input: &MeshEagerMetadataCacheInput,
) -> MeshEagerMetadataCachePlan {
    let (cache_status, local_body_hash, location, reason) = match input.local_body_hash.as_deref() {
        Some(hash) if hash == input.content_hash && input.policy_body_fetch_allowed => (
            MeshCacheStatus::Available,
            Some(hash.to_owned()),
            MeshCachedBodyLocation::CachedRemoteBody,
            "local_body_hash_matches_content_hash",
        ),
        Some(hash) => (
            MeshCacheStatus::Quarantined,
            Some(hash.to_owned()),
            MeshCachedBodyLocation::QuarantinedRemoteBody,
            "local_body_hash_mismatch_or_policy_denied",
        ),
        None if input.policy_body_fetch_allowed && input.body_ref_json.is_some() => (
            MeshCacheStatus::MetadataOnly,
            None,
            MeshCachedBodyLocation::FetchableRemoteBody,
            "metadata_cached_body_fetch_policy_allows_later_fetch",
        ),
        None => (
            MeshCacheStatus::MetadataOnly,
            None,
            MeshCachedBodyLocation::MetadataOnly,
            "metadata_cached_without_body",
        ),
    };
    let metadata_entry = MeshCacheEntry::derived(
        input.body_cache_key.clone(),
        input.peer_id.clone(),
        MeshCacheLane::Metadata,
        input.metadata_size_bytes,
    )
    .with_origin_workspace_id(input.origin_workspace_id.clone())
    .with_logical_memory_id(input.logical_memory_id.clone())
    .with_content_hash(input.content_hash.clone());

    MeshEagerMetadataCachePlan {
        metadata_entry,
        body_cache_key: input.body_cache_key.clone(),
        body_ref_json: input.body_ref_json.clone(),
        preview_hash: input.preview_hash.clone(),
        advertised_body_bytes: input.advertised_body_bytes,
        cache_status,
        local_body_hash: local_body_hash.clone(),
        metadata_index_allowed: input.policy_allows_metadata_index,
        body_fetch_allowed: input.policy_body_fetch_allowed,
        provenance: MeshCachedBodyProvenance {
            body_cache_key: input.body_cache_key.clone(),
            location,
            content_hash: input.content_hash.clone(),
            local_body_hash,
            freshness_ref: input.freshness_ref.clone(),
            reason,
        },
    }
}

#[must_use]
pub fn plan_policy_gated_lazy_body_fetch(
    input: &MeshLazyBodyFetchInput<'_>,
) -> MeshLazyBodyFetchPlan {
    if let Some(cached_hash) = input.cached_local_body_hash {
        if cached_hash == input.expected_content_hash {
            return lazy_body_plan(
                input,
                MeshLazyBodyFetchAction::UseCachedBody,
                MeshCacheStatus::Available,
                true,
                Some(cached_hash.to_owned()),
                MeshCachedBodyLocation::CachedRemoteBody,
                None,
                Vec::new(),
                "cached_body_hash_verified",
            );
        }
        return lazy_body_plan(
            input,
            MeshLazyBodyFetchAction::QuarantineBody,
            MeshCacheStatus::Quarantined,
            false,
            Some(cached_hash.to_owned()),
            MeshCachedBodyLocation::QuarantinedRemoteBody,
            Some("cached_body_hash_mismatch"),
            vec!["mesh_cached_body_hash_mismatch"],
            "cached_body_hash_mismatch",
        );
    }

    if !input.query_requires_body {
        return lazy_body_plan(
            input,
            MeshLazyBodyFetchAction::KeepMetadataOnly,
            MeshCacheStatus::MetadataOnly,
            false,
            None,
            MeshCachedBodyLocation::MetadataOnly,
            Some("lazy_body_not_required"),
            Vec::new(),
            "lazy_body_not_required",
        );
    }
    if !input.policy_body_fetch_allowed {
        return lazy_body_plan(
            input,
            MeshLazyBodyFetchAction::KeepMetadataOnly,
            MeshCacheStatus::MetadataOnly,
            false,
            None,
            MeshCachedBodyLocation::MetadataOnly,
            Some("body_fetch_denied_by_policy"),
            vec!["mesh_body_fetch_denied_by_policy"],
            "body_fetch_denied_by_policy",
        );
    }
    if !input.remote_body_available {
        return lazy_body_plan(
            input,
            MeshLazyBodyFetchAction::KeepMetadataOnly,
            MeshCacheStatus::MetadataOnly,
            false,
            None,
            MeshCachedBodyLocation::MetadataOnly,
            Some("remote_body_unavailable"),
            vec!["mesh_remote_body_unavailable"],
            "remote_body_unavailable",
        );
    }

    let Some(fetched_body) = input.fetched_body else {
        return lazy_body_plan(
            input,
            MeshLazyBodyFetchAction::FetchRemoteBody,
            MeshCacheStatus::MetadataOnly,
            false,
            None,
            MeshCachedBodyLocation::FetchableRemoteBody,
            None,
            Vec::new(),
            "body_fetch_allowed_pending_remote_read",
        );
    };
    let decision = decide_body_fetch_lifecycle(input.expected_content_hash, fetched_body);
    if decision.body_persist_allowed {
        lazy_body_plan(
            input,
            MeshLazyBodyFetchAction::FetchRemoteBody,
            MeshCacheStatus::Available,
            true,
            Some(decision.actual_local_body_hash),
            MeshCachedBodyLocation::CachedRemoteBody,
            None,
            Vec::new(),
            "fetched_body_hash_verified",
        )
    } else {
        lazy_body_plan(
            input,
            MeshLazyBodyFetchAction::QuarantineBody,
            MeshCacheStatus::Quarantined,
            false,
            Some(decision.actual_local_body_hash),
            MeshCachedBodyLocation::QuarantinedRemoteBody,
            Some("content_hash_mismatch"),
            vec!["mesh_fetched_body_hash_mismatch"],
            "fetched_body_hash_mismatch",
        )
    }
}

#[must_use]
pub fn decide_body_fetch_lifecycle(
    expected_content_hash: &str,
    fetched_body: &[u8],
) -> MeshCacheBodyFetchDecision {
    let actual_local_body_hash = blake3_content_hash(fetched_body);
    let hash_matches = expected_content_hash.starts_with("blake3:")
        && expected_content_hash == actual_local_body_hash;
    let (status, body_persist_allowed, quarantine_reason) = if hash_matches {
        (MeshCacheStatus::Available, true, None)
    } else {
        (
            MeshCacheStatus::Quarantined,
            false,
            Some("content_hash_mismatch".to_owned()),
        )
    };

    MeshCacheBodyFetchDecision {
        expected_content_hash: expected_content_hash.to_owned(),
        actual_local_body_hash,
        status,
        body_persist_allowed,
        quarantine_reason,
    }
}

#[must_use]
pub fn blake3_content_hash(body: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(body).to_hex())
}

fn lazy_body_plan(
    input: &MeshLazyBodyFetchInput<'_>,
    action: MeshLazyBodyFetchAction,
    cache_status: MeshCacheStatus,
    body_persist_allowed: bool,
    actual_local_body_hash: Option<String>,
    location: MeshCachedBodyLocation,
    denial_reason: Option<&'static str>,
    degraded_codes: Vec<&'static str>,
    provenance_reason: &'static str,
) -> MeshLazyBodyFetchPlan {
    MeshLazyBodyFetchPlan {
        action,
        cache_status,
        body_persist_allowed,
        actual_local_body_hash: actual_local_body_hash.clone(),
        denial_reason,
        degraded_codes,
        provenance: MeshCachedBodyProvenance {
            body_cache_key: input.body_cache_key.to_owned(),
            location,
            content_hash: input.expected_content_hash.to_owned(),
            local_body_hash: actual_local_body_hash,
            freshness_ref: input.freshness_ref.map(str::to_owned),
            reason: provenance_reason,
        },
    }
}

fn usage_for_all(entries: &[MeshCacheEntry]) -> MeshCacheUsage {
    let remaining = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.is_billable_cache().then_some(index))
        .collect::<BTreeSet<_>>();
    usage_for(entries, &remaining)
}

fn usage_for(entries: &[MeshCacheEntry], remaining: &BTreeSet<usize>) -> MeshCacheUsage {
    let mut usage = MeshCacheUsage::default();
    for index in remaining {
        usage.add(&entries[*index]);
    }
    usage
}

fn evict_index(
    index: usize,
    entries: &[MeshCacheEntry],
    remaining: &mut BTreeSet<usize>,
    evictions: &mut Vec<MeshCacheEviction>,
    reason: MeshCacheEvictionReason,
) {
    if !remaining.contains(&index) {
        return;
    }
    let before = usage_for(entries, remaining);
    remaining.remove(&index);
    let after = usage_for(entries, remaining);
    let entry = &entries[index];
    evictions.push(MeshCacheEviction {
        cache_key: entry.cache_key.clone(),
        peer_id: entry.peer_id.clone(),
        lane: entry.lane,
        bytes: entry.bytes,
        status_before: entry.status,
        status_after: MeshCacheStatus::Evicted,
        reason,
        audit_action: MESH_CACHE_EVICT_AUDIT_ACTION,
        cache_bytes_before: before.total_bytes,
        cache_bytes_after: after.total_bytes,
        evicted_count: 1,
    });
}

fn purge_index(
    index: usize,
    entries: &[MeshCacheEntry],
    remaining: &mut BTreeSet<usize>,
    evictions: &mut Vec<MeshCacheEviction>,
) -> bool {
    if !remaining.contains(&index) {
        return false;
    }
    let before = usage_for(entries, remaining);
    remaining.remove(&index);
    let after = usage_for(entries, remaining);
    let entry = &entries[index];
    evictions.push(MeshCacheEviction {
        cache_key: entry.cache_key.clone(),
        peer_id: entry.peer_id.clone(),
        lane: entry.lane,
        bytes: entry.bytes,
        status_before: entry.status,
        status_after: MeshCacheStatus::Evicted,
        reason: MeshCacheEvictionReason::ManualPurge,
        audit_action: MESH_CACHE_PURGE_AUDIT_ACTION,
        cache_bytes_before: before.total_bytes,
        cache_bytes_after: after.total_bytes,
        evicted_count: 1,
    });
    true
}

fn quota_violations(
    usage: &MeshCacheUsage,
    quotas: &MeshCacheQuotas,
) -> Vec<MeshCacheQuotaWarning> {
    quota_warnings_for_usage(usage, quotas, None)
}

fn quota_near_limit_warnings(
    usage: &MeshCacheUsage,
    quotas: &MeshCacheQuotas,
    warning_threshold_percent: u8,
) -> Vec<MeshCacheQuotaWarning> {
    if warning_threshold_percent == 0 {
        return Vec::new();
    }
    quota_warnings_for_usage(usage, quotas, Some(warning_threshold_percent.min(100)))
}

fn quota_warnings_for_usage(
    usage: &MeshCacheUsage,
    quotas: &MeshCacheQuotas,
    threshold_percent: Option<u8>,
) -> Vec<MeshCacheQuotaWarning> {
    let mut warnings = Vec::new();
    push_quota_warning(
        &mut warnings,
        MeshCacheQuotaKind::Global,
        None,
        None,
        usage.total_bytes,
        quotas.global_bytes,
        threshold_percent,
    );

    if let Some(limit) = quotas.per_peer_bytes {
        for (peer_id, bytes_after) in &usage.by_peer_bytes {
            push_quota_warning(
                &mut warnings,
                MeshCacheQuotaKind::Peer,
                Some(peer_id.clone()),
                None,
                *bytes_after,
                Some(limit),
                threshold_percent,
            );
        }
    }

    for lane in [
        MeshCacheLane::Metadata,
        MeshCacheLane::Body,
        MeshCacheLane::Embedding,
    ] {
        push_quota_warning(
            &mut warnings,
            MeshCacheQuotaKind::Lane,
            None,
            Some(lane),
            usage.bytes_for_lane(lane),
            quotas.lane_limit(lane),
            threshold_percent,
        );
    }

    warnings.sort_by(compare_quota_warning);
    warnings.dedup();
    warnings
}

fn push_quota_warning(
    warnings: &mut Vec<MeshCacheQuotaWarning>,
    kind: MeshCacheQuotaKind,
    peer_id: Option<String>,
    lane: Option<MeshCacheLane>,
    bytes_after: u64,
    limit_bytes: Option<u64>,
    threshold_percent: Option<u8>,
) {
    let Some(limit_bytes) = limit_bytes else {
        return;
    };
    let severity = if bytes_after > limit_bytes {
        MeshCacheQuotaWarningSeverity::WouldExceed
    } else if threshold_percent
        .is_some_and(|percent| crosses_threshold(bytes_after, limit_bytes, percent))
    {
        MeshCacheQuotaWarningSeverity::NearLimit
    } else {
        return;
    };
    warnings.push(MeshCacheQuotaWarning {
        kind,
        peer_id,
        lane,
        bytes_after,
        limit_bytes,
        severity,
    });
}

fn crosses_threshold(bytes_after: u64, limit_bytes: u64, threshold_percent: u8) -> bool {
    if limit_bytes == 0 {
        return bytes_after == 0;
    }
    u128::from(bytes_after) * 100 >= u128::from(limit_bytes) * u128::from(threshold_percent)
}

fn intersects_quota_violation(
    entry: &MeshCacheEntry,
    violations: &[MeshCacheQuotaWarning],
) -> bool {
    violations.iter().any(|violation| match violation.kind {
        MeshCacheQuotaKind::Global => true,
        MeshCacheQuotaKind::Peer => violation.peer_id.as_deref() == Some(entry.peer_id.as_str()),
        MeshCacheQuotaKind::Lane => violation.lane == Some(entry.lane),
    })
}

fn reason_for_quota_eviction(
    entry: &MeshCacheEntry,
    violations: &[MeshCacheQuotaWarning],
) -> MeshCacheEvictionReason {
    if violations.iter().any(|violation| {
        violation.kind == MeshCacheQuotaKind::Peer
            && violation.peer_id.as_deref() == Some(entry.peer_id.as_str())
    }) {
        return MeshCacheEvictionReason::PeerQuotaExceeded;
    }
    if violations.iter().any(|violation| {
        violation.kind == MeshCacheQuotaKind::Lane && violation.lane == Some(entry.lane)
    }) {
        return MeshCacheEvictionReason::LaneQuotaExceeded;
    }
    MeshCacheEvictionReason::GlobalQuotaExceeded
}

fn sort_eviction_candidates(candidates: &mut [usize], entries: &[MeshCacheEntry]) {
    candidates.sort_by(|left, right| compare_eviction_candidate(&entries[*left], &entries[*right]));
}

fn matches_withdrawal(entry: &MeshCacheEntry, input: &MeshWithdrawalPurgeInput) -> bool {
    entry.origin_workspace_id == input.origin_workspace_id
        && entry.logical_memory_id == input.logical_memory_id
}

fn sort_withdrawal_candidates(candidates: &mut [usize], entries: &[MeshCacheEntry]) {
    candidates.sort_by(|left, right| {
        let left = &entries[*left];
        let right = &entries[*right];
        left.peer_id
            .cmp(&right.peer_id)
            .then_with(|| left.lane.eviction_rank().cmp(&right.lane.eviction_rank()))
            .then_with(|| left.cache_key.cmp(&right.cache_key))
    });
}

fn compare_eviction_candidate(left: &MeshCacheEntry, right: &MeshCacheEntry) -> Ordering {
    left.retention_score
        .cmp(&right.retention_score)
        .then_with(|| left.last_access_seq.cmp(&right.last_access_seq))
        .then_with(|| left.lane.eviction_rank().cmp(&right.lane.eviction_rank()))
        .then_with(|| right.bytes.cmp(&left.bytes))
        .then_with(|| left.peer_id.cmp(&right.peer_id))
        .then_with(|| left.cache_key.cmp(&right.cache_key))
}

fn compare_quota_warning(left: &MeshCacheQuotaWarning, right: &MeshCacheQuotaWarning) -> Ordering {
    left.kind
        .as_str()
        .cmp(right.kind.as_str())
        .then_with(|| left.peer_id.cmp(&right.peer_id))
        .then_with(|| {
            left.lane
                .map(MeshCacheLane::as_str)
                .cmp(&right.lane.map(MeshCacheLane::as_str))
        })
        .then_with(|| left.bytes_after.cmp(&right.bytes_after))
        .then_with(|| left.limit_bytes.cmp(&right.limit_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_quota_uses_score_lru_then_lane_order() {
        let input = MeshCacheRetentionInput {
            entries: vec![
                MeshCacheEntry::derived("body-old", "peer_alpha", MeshCacheLane::Body, 200)
                    .with_retention_score(5)
                    .with_last_access_seq(1),
                MeshCacheEntry::derived("body-new", "peer_alpha", MeshCacheLane::Body, 200)
                    .with_retention_score(50)
                    .with_last_access_seq(5),
                MeshCacheEntry::derived("meta", "peer_alpha", MeshCacheLane::Metadata, 80)
                    .with_retention_score(5)
                    .with_last_access_seq(1),
            ],
            quotas: MeshCacheQuotas {
                global_bytes: Some(300),
                ..MeshCacheQuotas::unlimited()
            },
            now_epoch_ms: 1_000,
        };

        let plan = plan_mesh_cache_retention(&input);

        assert_eq!(plan.cache_bytes_before(), 480);
        assert_eq!(plan.cache_bytes_after(), 280);
        assert_eq!(plan.evicted_count(), 1);
        assert_eq!(plan.evictions[0].cache_key, "body-old");
        assert_eq!(
            plan.evictions[0].reason,
            MeshCacheEvictionReason::GlobalQuotaExceeded
        );
        assert_eq!(plan.evictions[0].cache_bytes_before, 480);
        assert_eq!(plan.evictions[0].cache_bytes_after, 280);
    }

    #[test]
    fn local_source_truth_is_not_billable_or_evictable() {
        let input = MeshCacheRetentionInput {
            entries: vec![
                MeshCacheEntry::local_source_truth("local-memory", MeshCacheLane::Body, 10_000),
                MeshCacheEntry::derived("peer-body", "peer_alpha", MeshCacheLane::Body, 100),
            ],
            quotas: MeshCacheQuotas {
                global_bytes: Some(50),
                ..MeshCacheQuotas::unlimited()
            },
            now_epoch_ms: 1_000,
        };

        let plan = plan_mesh_cache_retention(&input);

        assert_eq!(plan.protected_local_source_truth_count, 1);
        assert_eq!(plan.cache_bytes_before(), 100);
        assert_eq!(plan.cache_bytes_after(), 0);
        assert_eq!(plan.evictions[0].cache_key, "peer-body");
    }

    #[test]
    fn expired_entries_are_evicted_before_quota_sorting() {
        let input = MeshCacheRetentionInput {
            entries: vec![
                MeshCacheEntry::derived("expired-high", "peer_alpha", MeshCacheLane::Body, 100)
                    .with_retention_score(900)
                    .with_expires_at_epoch_ms(500),
                MeshCacheEntry::derived("fresh-low", "peer_alpha", MeshCacheLane::Body, 100)
                    .with_retention_score(1)
                    .with_expires_at_epoch_ms(5_000),
            ],
            quotas: MeshCacheQuotas::unlimited(),
            now_epoch_ms: 1_000,
        };

        let plan = plan_mesh_cache_retention(&input);

        assert_eq!(plan.evicted_count(), 1);
        assert_eq!(plan.evictions[0].cache_key, "expired-high");
        assert_eq!(plan.evictions[0].reason, MeshCacheEvictionReason::Expired);
        assert_eq!(plan.cache_bytes_after(), 100);
    }

    #[test]
    fn withdrawal_purges_body_and_embedding_but_keeps_metadata_tombstone() {
        let input = MeshWithdrawalPurgeInput {
            entries: vec![
                withdrawn_entry("meta-a", "peer_alpha", MeshCacheLane::Metadata, 20),
                withdrawn_entry("body-a", "peer_alpha", MeshCacheLane::Body, 200),
                withdrawn_entry("embed-a", "peer_alpha", MeshCacheLane::Embedding, 80),
                MeshCacheEntry::derived("body-other", "peer_alpha", MeshCacheLane::Body, 50)
                    .with_origin_workspace_id("wsp_remote")
                    .with_logical_memory_id("mem_other"),
            ],
            origin_workspace_id: "wsp_remote".to_owned(),
            logical_memory_id: "mem_withdrawn".to_owned(),
            peer_deliveries: vec![
                MeshWithdrawalPeerDelivery::reachable("peer_alpha"),
                MeshWithdrawalPeerDelivery::unreachable("peer_beta"),
            ],
        };

        let plan = plan_mesh_withdrawal_cache_purge(&input);

        assert_eq!(plan.schema, MESH_WITHDRAWAL_PURGE_SCHEMA_V1);
        assert_eq!(plan.cache_bytes_before, 350);
        assert_eq!(plan.cache_bytes_after, 70);
        assert_eq!(plan.purged_count(), 2);
        assert_eq!(plan.residual_metadata_count(), 1);
        assert_eq!(plan.residual_metadata[0].cache_key, "meta-a");
        assert_eq!(
            plan.residual_metadata[0].reason,
            WITHDRAWAL_RESIDUAL_METADATA_REASON
        );
        assert!(plan.evictions.iter().all(|eviction| {
            eviction.reason == MeshCacheEvictionReason::ManualPurge
                && eviction.audit_action == MESH_CACHE_PURGE_AUDIT_ACTION
        }));
        assert_eq!(
            plan.evictions
                .iter()
                .map(|eviction| eviction.cache_key.as_str())
                .collect::<Vec<_>>(),
            vec!["body-a", "embed-a"]
        );
        assert_eq!(
            plan.logs
                .iter()
                .map(|log| log.kind.as_str())
                .collect::<Vec<_>>(),
            vec![
                "withdrawal_event",
                "purge_requested",
                "purge_applied",
                "purge_applied",
                "residual_metadata_reason",
                "peer_unreachable"
            ]
        );
        assert_eq!(plan.replay_targets[0].peer_id, "peer_beta");
    }

    #[test]
    fn withdrawal_never_purges_local_source_truth_even_when_ids_match() {
        let input = MeshWithdrawalPurgeInput {
            entries: vec![
                MeshCacheEntry::local_source_truth("local-body", MeshCacheLane::Body, 500)
                    .with_origin_workspace_id("wsp_remote")
                    .with_logical_memory_id("mem_withdrawn"),
                withdrawn_entry("peer-body", "peer_alpha", MeshCacheLane::Body, 100),
            ],
            origin_workspace_id: "wsp_remote".to_owned(),
            logical_memory_id: "mem_withdrawn".to_owned(),
            peer_deliveries: Vec::new(),
        };

        let plan = plan_mesh_withdrawal_cache_purge(&input);

        assert_eq!(plan.protected_local_source_truth_count, 1);
        assert_eq!(plan.cache_bytes_before, 100);
        assert_eq!(plan.cache_bytes_after, 0);
        assert_eq!(plan.purged_count(), 1);
        assert_eq!(plan.evictions[0].cache_key, "peer-body");
    }

    #[test]
    fn unavailable_peers_are_sorted_and_marked_for_withdrawal_replay() {
        let input = MeshWithdrawalPurgeInput {
            entries: Vec::new(),
            origin_workspace_id: "wsp_remote".to_owned(),
            logical_memory_id: "mem_withdrawn".to_owned(),
            peer_deliveries: vec![
                MeshWithdrawalPeerDelivery::unreachable("peer_zulu"),
                MeshWithdrawalPeerDelivery::reachable("peer_alpha"),
                MeshWithdrawalPeerDelivery::unreachable("peer_beta"),
            ],
        };

        let plan = plan_mesh_withdrawal_cache_purge(&input);

        assert_eq!(
            plan.replay_targets
                .iter()
                .map(|target| target.peer_id.as_str())
                .collect::<Vec<_>>(),
            vec!["peer_beta", "peer_zulu"]
        );
        assert!(
            plan.replay_targets
                .iter()
                .all(|target| { target.reason == WITHDRAWAL_UNAVAILABLE_PEER_REPLAY_REASON })
        );
    }

    #[test]
    fn eager_metadata_cache_is_indexable_without_materializing_body() {
        let content_hash = blake3_content_hash(b"remote body");
        let plan = plan_eager_metadata_cache(&MeshEagerMetadataCacheInput {
            body_cache_key: "body-cache-alpha".to_owned(),
            peer_id: "peer_alpha".to_owned(),
            origin_workspace_id: "wsp_remote".to_owned(),
            logical_memory_id: "mem_remote".to_owned(),
            content_hash: content_hash.clone(),
            body_ref_json: Some(r#"{"kind":"remoteAvailable"}"#.to_owned()),
            preview_hash: Some(blake3_content_hash(b"preview")),
            metadata_size_bytes: 96,
            advertised_body_bytes: Some(11),
            policy_allows_metadata_index: true,
            policy_body_fetch_allowed: false,
            local_body_hash: None,
            freshness_ref: Some("seq:42".to_owned()),
        });

        assert_eq!(plan.metadata_entry.lane, MeshCacheLane::Metadata);
        assert_eq!(
            plan.metadata_entry.content_hash.as_deref(),
            Some(content_hash.as_str())
        );
        assert_eq!(plan.cache_status, MeshCacheStatus::MetadataOnly);
        assert!(plan.metadata_index_allowed);
        assert!(!plan.body_fetch_allowed);
        assert_eq!(plan.local_body_hash, None);
        assert_eq!(
            plan.provenance.location,
            MeshCachedBodyLocation::MetadataOnly
        );
        assert_eq!(plan.provenance.reason, "metadata_cached_without_body");
    }

    #[test]
    fn lazy_body_fetch_is_policy_gated_and_body_free_when_denied() {
        let plan = plan_policy_gated_lazy_body_fetch(&MeshLazyBodyFetchInput {
            body_cache_key: "body-cache-alpha",
            expected_content_hash: &blake3_content_hash(b"remote body"),
            policy_body_fetch_allowed: false,
            query_requires_body: true,
            remote_body_available: true,
            cached_local_body_hash: None,
            fetched_body: Some(b"remote body"),
            freshness_ref: Some("seq:42"),
        });

        assert_eq!(plan.action, MeshLazyBodyFetchAction::KeepMetadataOnly);
        assert_eq!(plan.cache_status, MeshCacheStatus::MetadataOnly);
        assert!(!plan.body_persist_allowed);
        assert_eq!(plan.actual_local_body_hash, None);
        assert_eq!(plan.denial_reason, Some("body_fetch_denied_by_policy"));
        assert_eq!(
            plan.degraded_codes,
            vec!["mesh_body_fetch_denied_by_policy"]
        );
        assert_eq!(
            plan.provenance.location,
            MeshCachedBodyLocation::MetadataOnly
        );
    }

    #[test]
    fn lazy_body_fetch_waits_until_body_is_needed() {
        let plan = plan_policy_gated_lazy_body_fetch(&MeshLazyBodyFetchInput {
            body_cache_key: "body-cache-alpha",
            expected_content_hash: &blake3_content_hash(b"remote body"),
            policy_body_fetch_allowed: true,
            query_requires_body: false,
            remote_body_available: true,
            cached_local_body_hash: None,
            fetched_body: Some(b"remote body"),
            freshness_ref: Some("seq:42"),
        });

        assert_eq!(plan.action, MeshLazyBodyFetchAction::KeepMetadataOnly);
        assert_eq!(plan.denial_reason, Some("lazy_body_not_required"));
        assert_eq!(plan.actual_local_body_hash, None);
    }

    #[test]
    fn lazy_body_fetch_verifies_hash_before_body_becomes_available() {
        let expected = blake3_content_hash(b"remote body");
        let plan = plan_policy_gated_lazy_body_fetch(&MeshLazyBodyFetchInput {
            body_cache_key: "body-cache-alpha",
            expected_content_hash: &expected,
            policy_body_fetch_allowed: true,
            query_requires_body: true,
            remote_body_available: true,
            cached_local_body_hash: None,
            fetched_body: Some(b"remote body"),
            freshness_ref: Some("seq:42"),
        });

        assert_eq!(plan.action, MeshLazyBodyFetchAction::FetchRemoteBody);
        assert_eq!(plan.cache_status, MeshCacheStatus::Available);
        assert!(plan.body_persist_allowed);
        assert_eq!(
            plan.actual_local_body_hash.as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            plan.provenance.location,
            MeshCachedBodyLocation::CachedRemoteBody
        );
    }

    #[test]
    fn lazy_body_fetch_quarantines_hash_mismatch() {
        let plan = plan_policy_gated_lazy_body_fetch(&MeshLazyBodyFetchInput {
            body_cache_key: "body-cache-alpha",
            expected_content_hash: &blake3_content_hash(b"expected body"),
            policy_body_fetch_allowed: true,
            query_requires_body: true,
            remote_body_available: true,
            cached_local_body_hash: None,
            fetched_body: Some(b"tampered body"),
            freshness_ref: Some("seq:42"),
        });

        assert_eq!(plan.action, MeshLazyBodyFetchAction::QuarantineBody);
        assert_eq!(plan.cache_status, MeshCacheStatus::Quarantined);
        assert!(!plan.body_persist_allowed);
        assert_eq!(plan.denial_reason, Some("content_hash_mismatch"));
        assert_eq!(plan.degraded_codes, vec!["mesh_fetched_body_hash_mismatch"]);
        assert_eq!(
            plan.provenance.location,
            MeshCachedBodyLocation::QuarantinedRemoteBody
        );
    }

    fn withdrawn_entry(
        cache_key: &str,
        peer_id: &str,
        lane: MeshCacheLane,
        bytes: u64,
    ) -> MeshCacheEntry {
        MeshCacheEntry::derived(cache_key, peer_id, lane, bytes)
            .with_origin_workspace_id("wsp_remote")
            .with_logical_memory_id("mem_withdrawn")
    }
}
