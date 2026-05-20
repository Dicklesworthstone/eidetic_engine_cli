//! SRR6.46.13 discovery-cache decision model.
//!
//! This module is pure policy logic for the Tailscale autodiscovery cache.
//! It does not read or write SQLite and does not probe the network. Callers
//! pass the current workspace/tailnet identity, the cache row they loaded,
//! and the reason a fresh probe may be required. The result tells the status
//! caller whether it can reuse the cached peer set or must refresh.

use serde::{Deserialize, Serialize};

pub const DISCOVERY_CACHE_SCHEMA_V1: &str = "ee.mesh.discovery_cache.v1";
pub const DEFAULT_DISCOVERY_CACHE_TTL_SECONDS: u64 = 30;
pub const DISCOVERY_CACHE_WORKSPACE_MISMATCH_CODE: &str =
    "discovery_cache_stale_due_to_workspace_mismatch";
pub const DISCOVERY_CACHE_TAILNET_CHANGED_CODE: &str =
    "discovery_cache_invalidated_tailnet_changed";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryCacheRefreshReason {
    Missing,
    TtlExpired,
    WorkspaceMismatch,
    TailnetChanged,
    ExplicitRefresh,
    AutoEnrollCompleted,
}

impl DiscoveryCacheRefreshReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::TtlExpired => "ttl_expired",
            Self::WorkspaceMismatch => "workspace_mismatch",
            Self::TailnetChanged => "tailnet_changed",
            Self::ExplicitRefresh => "explicit_refresh",
            Self::AutoEnrollCompleted => "auto_enroll_completed",
        }
    }

    #[must_use]
    pub const fn degraded_code(self) -> Option<&'static str> {
        match self {
            Self::WorkspaceMismatch => Some(DISCOVERY_CACHE_WORKSPACE_MISMATCH_CODE),
            Self::TailnetChanged => Some(DISCOVERY_CACHE_TAILNET_CHANGED_CODE),
            Self::Missing
            | Self::TtlExpired
            | Self::ExplicitRefresh
            | Self::AutoEnrollCompleted => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedDiscoveryPeer {
    pub peer_node_key: String,
    pub peer_group_id: String,
    pub magic_dns_name: Option<String>,
    pub endpoint: Option<String>,
    pub ee_protocol_version: Option<String>,
}

impl CachedDiscoveryPeer {
    #[must_use]
    pub fn new(peer_node_key: impl Into<String>, peer_group_id: impl Into<String>) -> Self {
        Self {
            peer_node_key: peer_node_key.into(),
            peer_group_id: peer_group_id.into(),
            magic_dns_name: None,
            endpoint: None,
            ee_protocol_version: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedDiscoverySkippedPeer {
    pub peer_node_key: String,
    pub reason: String,
}

impl CachedDiscoverySkippedPeer {
    #[must_use]
    pub fn new(peer_node_key: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            peer_node_key: peer_node_key.into(),
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshDiscoveryCacheEntry {
    pub schema: String,
    pub workspace_id: String,
    pub tailnet_id: String,
    pub cached_at_epoch_seconds: u64,
    pub valid_until_epoch_seconds: u64,
    pub peers: Vec<CachedDiscoveryPeer>,
    pub skipped_peers: Vec<CachedDiscoverySkippedPeer>,
}

impl MeshDiscoveryCacheEntry {
    #[must_use]
    pub fn fresh(
        workspace_id: impl Into<String>,
        tailnet_id: impl Into<String>,
        cached_at_epoch_seconds: u64,
        ttl_seconds: u64,
        peers: Vec<CachedDiscoveryPeer>,
        skipped_peers: Vec<CachedDiscoverySkippedPeer>,
    ) -> Self {
        Self {
            schema: DISCOVERY_CACHE_SCHEMA_V1.to_owned(),
            workspace_id: workspace_id.into(),
            tailnet_id: tailnet_id.into(),
            cached_at_epoch_seconds,
            valid_until_epoch_seconds: cached_at_epoch_seconds.saturating_add(ttl_seconds),
            peers,
            skipped_peers,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryCacheLookup<'a> {
    pub workspace_id: &'a str,
    pub tailnet_id: &'a str,
    pub now_epoch_seconds: u64,
    pub explicit_refresh: bool,
    pub auto_enroll_completed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryCacheDecision {
    Hit {
        entry: MeshDiscoveryCacheEntry,
    },
    Refresh {
        reason: DiscoveryCacheRefreshReason,
        stale_entry: Option<MeshDiscoveryCacheEntry>,
    },
}

impl DiscoveryCacheDecision {
    #[must_use]
    pub const fn hit(&self) -> bool {
        matches!(self, Self::Hit { .. })
    }

    #[must_use]
    pub const fn refreshed_reason(&self) -> Option<&'static str> {
        match self {
            Self::Hit { .. } => None,
            Self::Refresh { reason, .. } => Some(reason.as_str()),
        }
    }

    #[must_use]
    pub const fn degraded_code(&self) -> Option<&'static str> {
        match self {
            Self::Hit { .. } => None,
            Self::Refresh { reason, .. } => reason.degraded_code(),
        }
    }
}

#[must_use]
pub fn evaluate_discovery_cache(
    cached: Option<&MeshDiscoveryCacheEntry>,
    lookup: DiscoveryCacheLookup<'_>,
) -> DiscoveryCacheDecision {
    let Some(entry) = cached else {
        return DiscoveryCacheDecision::Refresh {
            reason: DiscoveryCacheRefreshReason::Missing,
            stale_entry: None,
        };
    };

    if lookup.auto_enroll_completed {
        return refresh_with(entry, DiscoveryCacheRefreshReason::AutoEnrollCompleted);
    }
    if lookup.explicit_refresh {
        return refresh_with(entry, DiscoveryCacheRefreshReason::ExplicitRefresh);
    }
    if entry.workspace_id != lookup.workspace_id {
        return refresh_with(entry, DiscoveryCacheRefreshReason::WorkspaceMismatch);
    }
    if entry.tailnet_id != lookup.tailnet_id {
        return refresh_with(entry, DiscoveryCacheRefreshReason::TailnetChanged);
    }
    if entry.valid_until_epoch_seconds <= lookup.now_epoch_seconds {
        return refresh_with(entry, DiscoveryCacheRefreshReason::TtlExpired);
    }

    DiscoveryCacheDecision::Hit {
        entry: entry.clone(),
    }
}

fn refresh_with(
    entry: &MeshDiscoveryCacheEntry,
    reason: DiscoveryCacheRefreshReason,
) -> DiscoveryCacheDecision {
    DiscoveryCacheDecision::Refresh {
        reason,
        stale_entry: Some(entry.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(ttl_seconds: u64) -> MeshDiscoveryCacheEntry {
        MeshDiscoveryCacheEntry::fresh(
            "wsp_alpha",
            "tailnet-alpha",
            100,
            ttl_seconds,
            vec![CachedDiscoveryPeer::new("nodekey:one", "pg_alpha")],
            vec![CachedDiscoverySkippedPeer::new(
                "nodekey:two",
                "missing_tag",
            )],
        )
    }

    fn lookup(now_epoch_seconds: u64) -> DiscoveryCacheLookup<'static> {
        DiscoveryCacheLookup {
            workspace_id: "wsp_alpha",
            tailnet_id: "tailnet-alpha",
            now_epoch_seconds,
            explicit_refresh: false,
            auto_enroll_completed: false,
        }
    }

    #[test]
    fn discovery_cache_returns_cached_result_on_back_to_back_status_within_ttl() {
        let entry = cache(DEFAULT_DISCOVERY_CACHE_TTL_SECONDS);
        let decision = evaluate_discovery_cache(Some(&entry), lookup(101));

        assert!(decision.hit());
        assert_eq!(decision.refreshed_reason(), None);
        match decision {
            DiscoveryCacheDecision::Hit { entry } => {
                assert_eq!(entry.peers.len(), 1);
                assert_eq!(entry.skipped_peers.len(), 1);
            }
            DiscoveryCacheDecision::Refresh { .. } => panic!("expected cache hit"),
        }
    }

    #[test]
    fn discovery_cache_refreshes_on_ttl_expiry() {
        let entry = cache(30);
        let decision = evaluate_discovery_cache(Some(&entry), lookup(130));

        assert!(!decision.hit());
        assert_eq!(decision.refreshed_reason(), Some("ttl_expired"));
        assert_eq!(decision.degraded_code(), None);
    }

    #[test]
    fn discovery_cache_invalidated_on_tailnet_id_change() {
        let entry = cache(30);
        let decision = evaluate_discovery_cache(
            Some(&entry),
            DiscoveryCacheLookup {
                tailnet_id: "tailnet-beta",
                ..lookup(101)
            },
        );

        assert_eq!(decision.refreshed_reason(), Some("tailnet_changed"));
        assert_eq!(
            decision.degraded_code(),
            Some("discovery_cache_invalidated_tailnet_changed")
        );
    }

    #[test]
    fn discovery_cache_invalidated_on_explicit_refresh_flag() {
        let entry = cache(30);
        let decision = evaluate_discovery_cache(
            Some(&entry),
            DiscoveryCacheLookup {
                explicit_refresh: true,
                ..lookup(101)
            },
        );

        assert_eq!(decision.refreshed_reason(), Some("explicit_refresh"));
    }

    #[test]
    fn discovery_cache_invalidated_after_auto_enroll_completes() {
        let entry = cache(30);
        let decision = evaluate_discovery_cache(
            Some(&entry),
            DiscoveryCacheLookup {
                auto_enroll_completed: true,
                ..lookup(101)
            },
        );

        assert_eq!(decision.refreshed_reason(), Some("auto_enroll_completed"));
    }

    #[test]
    fn discovery_cache_invalidated_on_workspace_id_change() {
        let entry = cache(30);
        let decision = evaluate_discovery_cache(
            Some(&entry),
            DiscoveryCacheLookup {
                workspace_id: "wsp_beta",
                ..lookup(101)
            },
        );

        assert_eq!(decision.refreshed_reason(), Some("workspace_mismatch"));
        assert_eq!(
            decision.degraded_code(),
            Some("discovery_cache_stale_due_to_workspace_mismatch")
        );
    }
}
