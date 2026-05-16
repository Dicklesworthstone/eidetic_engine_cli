//! Audit-event payload builders for graph operations (bd-bife.15).
//!
//! The `audit_log` table is `ee`'s compliance trail. Every state-
//! changing operation should emit a row so an operator can answer
//! "which agent touched this snapshot at what time and why?" The
//! graph subsystem has historically NOT emitted any audit rows
//! even though it mutates persisted state (`graph_snapshots`,
//! `graph_algorithm_witnesses`, `graph_algorithm_results`). This
//! module defines the canonical action names plus typed payload
//! builders for the five graph audit actions; call-site wiring
//! into F1 snapshot lifecycle and F2 cache/algorithm wrappers is
//! tracked separately so this slice can land clean against an
//! uncontested file boundary.
//!
//! ## Action vocabulary (matches bd-bife.15 verbatim)
//!
//! | Action constant | Target type | Target id | Mutation kind |
//! | --- | --- | --- | --- |
//! | [`SNAPSHOT_REFRESHED_ACTION`]    | `graph_snapshot`           | snapshot id | `state_change` |
//! | [`SNAPSHOT_ARCHIVED_ACTION`]     | `graph_snapshot`           | snapshot id | `state_change` |
//! | [`RESULT_CACHED_ACTION`]         | `graph_algorithm_witness`  | witness id  | `derived_write` |
//! | [`RESULT_EVICTED_ACTION`]        | `graph_algorithm_witness`  | witness id  | `derived_delete` |
//! | [`ALGORITHM_DEGRADED_ACTION`]    | `graph_algorithm`          | algorithm   | `degraded_emit` |
//!
//! The builders produce a stable `details` JSON object whose keys
//! match the bd-bife.15 spec. Callers serialize the returned
//! [`GraphAuditPayload`] into the `audit_log` row's `details`
//! column unchanged; the JSON is deterministic across runs (sorted
//! BTreeMap-backed for the details body, fixed key ordering inside
//! each variant) so test goldens stay byte-stable.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

/// Audit `action` value for "a persisted graph snapshot was just
/// rebuilt". Target is the `graph_snapshot` row id.
pub const SNAPSHOT_REFRESHED_ACTION: &str = "graph.snapshot.refreshed";

/// Audit `action` value for "a graph snapshot was archived (made
/// inactive)" — either by a newer snapshot superseding it or by
/// an operator request. Target is the archived snapshot's id.
pub const SNAPSHOT_ARCHIVED_ACTION: &str = "graph.snapshot.archived";

/// Audit `action` value for "an algorithm computation produced a
/// witness row that was inserted into the algorithm-result cache".
/// Target is the new witness id.
pub const RESULT_CACHED_ACTION: &str = "graph.algorithm.result_cached";

/// Audit `action` value for "a cached algorithm-result was
/// evicted" — either because the underlying snapshot was
/// invalidated or because the row aged past its TTL. Target is
/// the evicted witness id.
pub const RESULT_EVICTED_ACTION: &str = "graph.algorithm.result_evicted";

/// Audit `action` value for "an algorithm emitted a degradation
/// code on a request" (separate from non-graph degradations). The
/// target is the algorithm name; the `details` carry the code and
/// remediation hint so a compliance review can reconstruct what
/// the operator was told.
pub const ALGORITHM_DEGRADED_ACTION: &str = "graph.algorithm.degraded";

/// `target_type` value used for snapshot-lifecycle audit rows.
pub const SNAPSHOT_TARGET_TYPE: &str = "graph_snapshot";

/// `target_type` value used for cache lifecycle audit rows.
pub const WITNESS_TARGET_TYPE: &str = "graph_algorithm_witness";

/// `target_type` value used for the degraded-emit audit row whose
/// target is the algorithm itself rather than a row id.
pub const ALGORITHM_TARGET_TYPE: &str = "graph_algorithm";

/// `mutation_kind` value used for snapshot-lifecycle rows.
pub const STATE_CHANGE_MUTATION_KIND: &str = "state_change";

/// `mutation_kind` value used for cache insertions.
pub const DERIVED_WRITE_MUTATION_KIND: &str = "derived_write";

/// `mutation_kind` value used for cache evictions.
pub const DERIVED_DELETE_MUTATION_KIND: &str = "derived_delete";

/// `mutation_kind` value used for degraded-emission rows.
pub const DEGRADED_EMIT_MUTATION_KIND: &str = "degraded_emit";

/// Canonical sweep list of every graph audit action this module
/// defines. The documentation generator and the wiring-coverage
/// test both walk this slice, so adding a new action automatically
/// gains downstream coverage without re-edits elsewhere.
pub const ALL_GRAPH_AUDIT_ACTIONS: &[&str] = &[
    SNAPSHOT_REFRESHED_ACTION,
    SNAPSHOT_ARCHIVED_ACTION,
    RESULT_CACHED_ACTION,
    RESULT_EVICTED_ACTION,
    ALGORITHM_DEGRADED_ACTION,
];

/// Stable reason values for [`build_snapshot_archived_payload`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotArchivedReason {
    /// A newer snapshot superseded this one.
    NewerSnapshot,
    /// An operator explicitly archived this snapshot.
    Manual,
}

impl SnapshotArchivedReason {
    /// Stable wire string emitted into the `details.reason` field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewerSnapshot => "newer_snapshot",
            Self::Manual => "manual",
        }
    }
}

/// Stable reason values for [`build_result_evicted_payload`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultEvictedReason {
    /// The snapshot the row was tied to has been invalidated /
    /// archived, so its cached results are no longer addressable.
    SnapshotInvalidated,
    /// The cached row aged past its TTL.
    TtlExpired,
}

impl ResultEvictedReason {
    /// Stable wire string emitted into the `details.reason` field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SnapshotInvalidated => "snapshot_invalidated",
            Self::TtlExpired => "ttl_expired",
        }
    }
}

/// A fully-assembled graph audit row, ready for the call site to
/// pass to the `audit_log` writer. The builder fills in everything
/// that's a pure function of the trigger inputs; the caller
/// supplies `id`, `workspace_id`, `timestamp`, and `actor` since
/// those are properties of the surrounding transaction rather
/// than the graph event itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphAuditPayload {
    pub action: &'static str,
    pub target_type: &'static str,
    pub target_id: String,
    pub mutation_kind: &'static str,
    pub details: Value,
}

/// Inputs for a [`SNAPSHOT_REFRESHED_ACTION`] payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotRefreshedInputs<'a> {
    pub snapshot_id: &'a str,
    pub graph_type: &'a str,
    pub snapshot_version: u64,
    pub content_hash: &'a str,
    pub build_time_ms: u64,
    pub node_count: usize,
    pub edge_count: usize,
}

/// Inputs for a [`SNAPSHOT_ARCHIVED_ACTION`] payload. `archived_at`
/// is the RFC 3339 timestamp the caller already recorded for the
/// archival; passing it explicitly keeps the audit row identical
/// to the row written into the snapshot table itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotArchivedInputs<'a> {
    pub snapshot_id: &'a str,
    pub archived_at: &'a str,
    pub reason: SnapshotArchivedReason,
}

/// Inputs for a [`RESULT_CACHED_ACTION`] payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultCachedInputs<'a> {
    pub witness_id: &'a str,
    pub algorithm: &'a str,
    pub params_hash: &'a str,
    pub elapsed_ms: u64,
    pub cache_size_after: u64,
}

/// Inputs for a [`RESULT_EVICTED_ACTION`] payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultEvictedInputs<'a> {
    pub witness_id: &'a str,
    pub reason: ResultEvictedReason,
}

/// Inputs for an [`ALGORITHM_DEGRADED_ACTION`] payload. `snapshot_version`
/// is `None` when the algorithm degraded before resolving a
/// snapshot (e.g. snapshot unavailable). The `repair` field
/// mirrors what the operator saw in the response envelope so an
/// audit reader can correlate the row with the user-facing
/// remediation hint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlgorithmDegradedInputs<'a> {
    pub algorithm: &'a str,
    pub code: &'a str,
    pub severity: &'a str,
    pub repair: &'a str,
    pub snapshot_version: Option<u64>,
}

/// Build a [`SNAPSHOT_REFRESHED_ACTION`] payload from typed inputs.
#[must_use]
pub fn build_snapshot_refreshed_payload(inputs: SnapshotRefreshedInputs<'_>) -> GraphAuditPayload {
    let mut details = BTreeMap::new();
    details.insert(
        "graph_type".to_owned(),
        Value::String(inputs.graph_type.to_owned()),
    );
    details.insert(
        "snapshot_version".to_owned(),
        Value::Number(inputs.snapshot_version.into()),
    );
    details.insert(
        "content_hash".to_owned(),
        Value::String(inputs.content_hash.to_owned()),
    );
    details.insert(
        "build_time_ms".to_owned(),
        Value::Number(inputs.build_time_ms.into()),
    );
    details.insert(
        "node_count".to_owned(),
        Value::Number(serde_json::Number::from(inputs.node_count as u64)),
    );
    details.insert(
        "edge_count".to_owned(),
        Value::Number(serde_json::Number::from(inputs.edge_count as u64)),
    );
    GraphAuditPayload {
        action: SNAPSHOT_REFRESHED_ACTION,
        target_type: SNAPSHOT_TARGET_TYPE,
        target_id: inputs.snapshot_id.to_owned(),
        mutation_kind: STATE_CHANGE_MUTATION_KIND,
        details: serde_json::to_value(details).expect("BTreeMap<String, Value> serializes"),
    }
}

/// Build a [`SNAPSHOT_ARCHIVED_ACTION`] payload from typed inputs.
#[must_use]
pub fn build_snapshot_archived_payload(inputs: SnapshotArchivedInputs<'_>) -> GraphAuditPayload {
    let details = json!({
        "archived_at": inputs.archived_at,
        "reason": inputs.reason.as_str(),
    });
    GraphAuditPayload {
        action: SNAPSHOT_ARCHIVED_ACTION,
        target_type: SNAPSHOT_TARGET_TYPE,
        target_id: inputs.snapshot_id.to_owned(),
        mutation_kind: STATE_CHANGE_MUTATION_KIND,
        details,
    }
}

/// Build a [`RESULT_CACHED_ACTION`] payload from typed inputs.
#[must_use]
pub fn build_result_cached_payload(inputs: ResultCachedInputs<'_>) -> GraphAuditPayload {
    let details = json!({
        "algorithm": inputs.algorithm,
        "params_hash": inputs.params_hash,
        "elapsed_ms": inputs.elapsed_ms,
        "cache_size_after": inputs.cache_size_after,
    });
    GraphAuditPayload {
        action: RESULT_CACHED_ACTION,
        target_type: WITNESS_TARGET_TYPE,
        target_id: inputs.witness_id.to_owned(),
        mutation_kind: DERIVED_WRITE_MUTATION_KIND,
        details,
    }
}

/// Build a [`RESULT_EVICTED_ACTION`] payload from typed inputs.
#[must_use]
pub fn build_result_evicted_payload(inputs: ResultEvictedInputs<'_>) -> GraphAuditPayload {
    let details = json!({
        "reason": inputs.reason.as_str(),
    });
    GraphAuditPayload {
        action: RESULT_EVICTED_ACTION,
        target_type: WITNESS_TARGET_TYPE,
        target_id: inputs.witness_id.to_owned(),
        mutation_kind: DERIVED_DELETE_MUTATION_KIND,
        details,
    }
}

/// Build an [`ALGORITHM_DEGRADED_ACTION`] payload from typed inputs.
#[must_use]
pub fn build_algorithm_degraded_payload(inputs: AlgorithmDegradedInputs<'_>) -> GraphAuditPayload {
    let mut details = serde_json::Map::new();
    details.insert("code".to_owned(), Value::String(inputs.code.to_owned()));
    details.insert(
        "severity".to_owned(),
        Value::String(inputs.severity.to_owned()),
    );
    details.insert("repair".to_owned(), Value::String(inputs.repair.to_owned()));
    if let Some(snapshot_version) = inputs.snapshot_version {
        details.insert(
            "snapshot_version".to_owned(),
            Value::Number(snapshot_version.into()),
        );
    }
    GraphAuditPayload {
        action: ALGORITHM_DEGRADED_ACTION,
        target_type: ALGORITHM_TARGET_TYPE,
        target_id: inputs.algorithm.to_owned(),
        mutation_kind: DEGRADED_EMIT_MUTATION_KIND,
        details: Value::Object(details),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every canonical action uses the documented `graph.`
    /// namespace, so a downstream `ee audit query --action graph.*`
    /// filter scopes correctly without per-action aliases.
    #[test]
    fn all_actions_share_the_graph_namespace_prefix() {
        for action in ALL_GRAPH_AUDIT_ACTIONS {
            assert!(
                action.starts_with("graph."),
                "{action} must start with the graph. namespace"
            );
        }
        assert_eq!(
            ALL_GRAPH_AUDIT_ACTIONS.len(),
            5,
            "bd-bife.15 declares five graph audit actions"
        );
    }

    /// Every action name in the canonical list is unique.
    #[test]
    fn all_action_names_are_unique() {
        let mut seen: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
        for action in ALL_GRAPH_AUDIT_ACTIONS {
            assert!(seen.insert(action), "duplicate action {action}");
        }
    }

    #[test]
    fn snapshot_refreshed_payload_carries_documented_keys_and_values() {
        let payload = build_snapshot_refreshed_payload(SnapshotRefreshedInputs {
            snapshot_id: "snap_01HX",
            graph_type: "memory_links",
            snapshot_version: 42,
            content_hash: "blake3:deadbeef",
            build_time_ms: 18,
            node_count: 14_000,
            edge_count: 35_000,
        });
        assert_eq!(payload.action, SNAPSHOT_REFRESHED_ACTION);
        assert_eq!(payload.target_type, SNAPSHOT_TARGET_TYPE);
        assert_eq!(payload.target_id, "snap_01HX");
        assert_eq!(payload.mutation_kind, STATE_CHANGE_MUTATION_KIND);
        let details = payload.details.as_object().expect("object");
        assert_eq!(details.get("graph_type"), Some(&json!("memory_links")));
        assert_eq!(details.get("snapshot_version"), Some(&json!(42_u64)));
        assert_eq!(details.get("content_hash"), Some(&json!("blake3:deadbeef")));
        assert_eq!(details.get("build_time_ms"), Some(&json!(18_u64)));
        assert_eq!(details.get("node_count"), Some(&json!(14_000_u64)));
        assert_eq!(details.get("edge_count"), Some(&json!(35_000_u64)));
        // The object must not carry any other keys — extra fields
        // would silently leak into the audit log without docs.
        assert_eq!(details.len(), 6, "exactly six documented keys");
    }

    #[test]
    fn snapshot_archived_payload_handles_both_reasons() {
        for reason in [
            SnapshotArchivedReason::NewerSnapshot,
            SnapshotArchivedReason::Manual,
        ] {
            let payload = build_snapshot_archived_payload(SnapshotArchivedInputs {
                snapshot_id: "snap_old",
                archived_at: "2026-05-16T07:00:00Z",
                reason,
            });
            assert_eq!(payload.action, SNAPSHOT_ARCHIVED_ACTION);
            assert_eq!(payload.target_id, "snap_old");
            let details = payload.details.as_object().expect("object");
            assert_eq!(
                details.get("archived_at"),
                Some(&json!("2026-05-16T07:00:00Z"))
            );
            assert_eq!(details.get("reason"), Some(&json!(reason.as_str())));
        }
    }

    #[test]
    fn result_cached_payload_records_cache_size_after_for_correlation() {
        let payload = build_result_cached_payload(ResultCachedInputs {
            witness_id: "audit_01HX",
            algorithm: "personalized_pagerank",
            params_hash: "blake3:cafe",
            elapsed_ms: 27,
            cache_size_after: 1_234,
        });
        assert_eq!(payload.action, RESULT_CACHED_ACTION);
        assert_eq!(payload.target_type, WITNESS_TARGET_TYPE);
        assert_eq!(payload.target_id, "audit_01HX");
        assert_eq!(payload.mutation_kind, DERIVED_WRITE_MUTATION_KIND);
        let details = payload.details.as_object().expect("object");
        assert_eq!(
            details.get("algorithm"),
            Some(&json!("personalized_pagerank"))
        );
        assert_eq!(details.get("params_hash"), Some(&json!("blake3:cafe")));
        assert_eq!(details.get("elapsed_ms"), Some(&json!(27_u64)));
        assert_eq!(details.get("cache_size_after"), Some(&json!(1_234_u64)));
    }

    #[test]
    fn result_evicted_payload_records_both_documented_reasons() {
        for reason in [
            ResultEvictedReason::SnapshotInvalidated,
            ResultEvictedReason::TtlExpired,
        ] {
            let payload = build_result_evicted_payload(ResultEvictedInputs {
                witness_id: "audit_01HY",
                reason,
            });
            assert_eq!(payload.action, RESULT_EVICTED_ACTION);
            assert_eq!(payload.mutation_kind, DERIVED_DELETE_MUTATION_KIND);
            let details = payload.details.as_object().expect("object");
            assert_eq!(details.get("reason"), Some(&json!(reason.as_str())));
        }
    }

    #[test]
    fn algorithm_degraded_payload_omits_snapshot_version_when_unresolved() {
        let payload = build_algorithm_degraded_payload(AlgorithmDegradedInputs {
            algorithm: "louvain",
            code: "snapshot_unavailable",
            severity: "warning",
            repair: "ee graph snapshot refresh --workspace .",
            snapshot_version: None,
        });
        assert_eq!(payload.action, ALGORITHM_DEGRADED_ACTION);
        assert_eq!(payload.target_type, ALGORITHM_TARGET_TYPE);
        assert_eq!(payload.target_id, "louvain");
        assert_eq!(payload.mutation_kind, DEGRADED_EMIT_MUTATION_KIND);
        let details = payload.details.as_object().expect("object");
        assert_eq!(details.get("code"), Some(&json!("snapshot_unavailable")));
        assert_eq!(details.get("severity"), Some(&json!("warning")));
        assert_eq!(
            details.get("repair"),
            Some(&json!("ee graph snapshot refresh --workspace ."))
        );
        assert!(
            !details.contains_key("snapshot_version"),
            "snapshot_version is omitted when the algorithm degraded before resolving one"
        );
    }

    #[test]
    fn algorithm_degraded_payload_includes_snapshot_version_when_known() {
        let payload = build_algorithm_degraded_payload(AlgorithmDegradedInputs {
            algorithm: "ppr",
            code: "snapshot_stale",
            severity: "medium",
            repair: "ee graph snapshot refresh --workspace .",
            snapshot_version: Some(7),
        });
        let details = payload.details.as_object().expect("object");
        assert_eq!(details.get("snapshot_version"), Some(&json!(7_u64)));
    }

    /// Each `SnapshotArchivedReason::as_str` value is unique and
    /// lower_snake_case (so log-query languages match without case
    /// folding).
    #[test]
    fn snapshot_archived_reason_wire_strings_are_unique_and_snake_case() {
        let strings = [
            SnapshotArchivedReason::NewerSnapshot.as_str(),
            SnapshotArchivedReason::Manual.as_str(),
        ];
        for string in strings {
            assert!(string.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
            assert!(!string.is_empty());
        }
        let unique: std::collections::BTreeSet<&str> = strings.iter().copied().collect();
        assert_eq!(unique.len(), strings.len());
    }

    /// Same coverage for [`ResultEvictedReason`].
    #[test]
    fn result_evicted_reason_wire_strings_are_unique_and_snake_case() {
        let strings = [
            ResultEvictedReason::SnapshotInvalidated.as_str(),
            ResultEvictedReason::TtlExpired.as_str(),
        ];
        for string in strings {
            assert!(string.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
            assert!(!string.is_empty());
        }
        let unique: std::collections::BTreeSet<&str> = strings.iter().copied().collect();
        assert_eq!(unique.len(), strings.len());
    }

    /// Builders are deterministic — same inputs always produce
    /// byte-identical JSON. Critical for J7 since the audit row's
    /// `details` column is the source of truth for compliance
    /// queries.
    #[test]
    fn payloads_serialize_byte_stable_across_runs() {
        let first = build_snapshot_refreshed_payload(SnapshotRefreshedInputs {
            snapshot_id: "snap_det",
            graph_type: "memory_links",
            snapshot_version: 9,
            content_hash: "blake3:0000",
            build_time_ms: 4,
            node_count: 10,
            edge_count: 20,
        });
        let second = build_snapshot_refreshed_payload(SnapshotRefreshedInputs {
            snapshot_id: "snap_det",
            graph_type: "memory_links",
            snapshot_version: 9,
            content_hash: "blake3:0000",
            build_time_ms: 4,
            node_count: 10,
            edge_count: 20,
        });
        let json_first = serde_json::to_string(&first).expect("serialize");
        let json_second = serde_json::to_string(&second).expect("serialize");
        assert_eq!(json_first, json_second);
    }
}
