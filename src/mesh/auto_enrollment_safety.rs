//! SRR6.46.5 — Auto-enrollment safety snapshot.
//!
//! Forensic audit row emitted by `ee mesh auto-enroll` BEFORE any durable
//! peer-group write. The audit row captures exactly what the caller intends
//! to enroll, so a user reviewing the audit timeline can see every attempted
//! enrollment (materialized, dry-run, refused, or audit-only).
//!
//! Schema: `ee.mesh.auto_enrollment_summary.v1` at
//! `docs/schemas/ee.mesh.auto_enrollment_summary.v1.json`.
//!
//! Invariants (per the bead description):
//!
//! - The audit row is always emitted before SRR6.46.3 mutates the peer-group
//!   table. SRR6.46.3 calls [`emit_safety_snapshot_audit`] FIRST; on any
//!   audit-write error it aborts with `auto_enrollment_audit_failed`
//!   (critical) and writes nothing else. Fail-closed.
//! - The audit row is even emitted for `--dry-run` invocations (with
//!   `triggerReason = dry_run_preview`, `materializationOutcome = dry_run`)
//!   so the forensic log records every attempted enrollment.
//! - The `summaryHash` is `blake3` of the canonical JSON of every other
//!   summary field. It is stable across repeat serialization and supports
//!   forensic correlation across audit chains, handoff capsules, support
//!   bundles, and the SRR6.46.9 rollback path's `previousSummaryHash`.
//! - The `materializationOutcome` is back-filled by SRR6.46.3 on the same
//!   audit row's `details` JSON via [`update_materialization_outcome`].
//!   That keeps the audit-row count = attempt count without spawning a
//!   second row for each outcome.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::db::{CreateAuditInput, DbConnection, audit_actions, generate_audit_id};
use crate::models::DomainError;

/// JSON schema identifier emitted by [`AutoEnrollmentSummary::SCHEMA`].
pub const AUTO_ENROLLMENT_SUMMARY_SCHEMA_V1: &str = "ee.mesh.auto_enrollment_summary.v1";

/// Trigger that caused this snapshot.
///
/// `tailnet_changed_refusal` and `dry_run_preview` are forensic-only — the
/// audit row is emitted but no peer-group write follows.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerReason {
    FirstRun,
    ManualInvoke,
    StewardPass,
    DriftReconciliation,
    TailnetChangedRefusal,
    DryRunPreview,
}

impl TriggerReason {
    /// Canonical schema representation (matches the `triggerReason` enum
    /// values in `ee.mesh.auto_enrollment_summary.v1`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstRun => "first_run",
            Self::ManualInvoke => "manual_invoke",
            Self::StewardPass => "steward_pass",
            Self::DriftReconciliation => "drift_reconciliation",
            Self::TailnetChangedRefusal => "tailnet_changed_refusal",
            Self::DryRunPreview => "dry_run_preview",
        }
    }
}

/// Outcome state for the audit row. Back-filled by SRR6.46.3 once it
/// finishes (or fails) the peer-group write.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationOutcome {
    /// Initial value at audit-emit time, before SRR6.46.3 decides.
    Pending,
    /// SRR6.46.3 wrote the peer-group row.
    Materialized,
    /// SRR6.46.9 reversed a prior materialization.
    RolledBack,
    /// `--dry-run`: audit emitted as a record of intent, no write followed.
    DryRun,
    /// Idempotent path: peer set matched the existing materialization,
    /// audit emitted under `drift_reconciliation`, no peer-group write.
    AuditOnly,
}

impl MaterializationOutcome {
    /// Canonical schema representation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Materialized => "materialized",
            Self::RolledBack => "rolled_back",
            Self::DryRun => "dry_run",
            Self::AuditOnly => "audit_only",
        }
    }
}

/// Which SRR6.46.7 discovery-policy mode admitted a peer.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryPolicyDecision {
    AutoAdmit,
    ServiceTagMatch,
    Allowlisted,
}

impl DiscoveryPolicyDecision {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoAdmit => "auto_admit",
            Self::ServiceTagMatch => "service_tag_match",
            Self::Allowlisted => "allowlisted",
        }
    }
}

/// Per-lane decision (matches `laneDecision` in
/// `ee.mesh.peer_group_binding.v1`). The conservative default policy SRR6.46.3
/// produces is `metadata=allow, revisionNotice=allow, curationSignal=allow,
/// body=deny, embedding=deny, graphLink=deny`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneDecision {
    Allow,
    Quarantine,
    Deny,
}

impl LaneDecision {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Quarantine => "quarantine",
            Self::Deny => "deny",
        }
    }
}

/// One intended peer in the snapshot.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct IntendedPeer {
    pub node_key: String,
    pub tailscale_ip: String,
    pub magic_dns_name: Option<String>,
    pub hostname: String,
    pub ee_protocol_version: String,
    pub discovery_policy_decision: DiscoveryPolicyDecision,
}

/// Conservative-default lane policy emitted by SRR6.46.3 for fresh
/// auto-enrollment.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct IntendedLanePolicy {
    pub metadata: LaneDecision,
    pub body: LaneDecision,
    pub embedding: LaneDecision,
    pub graph_link: LaneDecision,
    pub revision_notice: LaneDecision,
    pub curation_signal: LaneDecision,
}

impl IntendedLanePolicy {
    /// The canonical conservative defaults SRR6.46.3 ships with.
    #[must_use]
    pub fn conservative_default() -> Self {
        Self {
            metadata: LaneDecision::Allow,
            body: LaneDecision::Deny,
            embedding: LaneDecision::Deny,
            graph_link: LaneDecision::Deny,
            revision_notice: LaneDecision::Allow,
            curation_signal: LaneDecision::Allow,
        }
    }
}

/// Caller-friendly input to [`compute_summary`].
///
/// The caller supplies the materialization context; this module computes
/// the derived hash fields, fills in the schema constant, and produces the
/// canonical [`AutoEnrollmentSummary`] payload ready for emission.
#[derive(Debug, Clone)]
pub struct AutoEnrollmentSummaryInput<'a> {
    pub workspace_id: &'a str,
    pub workspace_path: &'a str,
    pub tailnet_id: &'a str,
    pub tailnet_display_name: Option<&'a str>,
    pub intended_peers: Vec<IntendedPeer>,
    pub intended_lane_policy: IntendedLanePolicy,
    pub trigger_reason: TriggerReason,
    pub previous_peer_group_id: Option<&'a str>,
    pub previous_peer_set_hash: Option<&'a str>,
    pub materialization_outcome: MaterializationOutcome,
}

/// Canonical schema-shape struct emitted into the audit row's `details` JSON.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutoEnrollmentSummary {
    pub schema: &'static str,
    #[serde(rename = "workspaceId")]
    pub workspace_id: String,
    #[serde(rename = "tailnetId")]
    pub tailnet_id: String,
    #[serde(rename = "tailnetDisplayName", skip_serializing_if = "Option::is_none")]
    pub tailnet_display_name: Option<String>,
    #[serde(rename = "intendedPeers")]
    pub intended_peers: Vec<IntendedPeerJson>,
    #[serde(rename = "intendedLanePolicy")]
    pub intended_lane_policy: IntendedLanePolicyJson,
    #[serde(rename = "triggerReason")]
    pub trigger_reason: String,
    #[serde(
        rename = "previousPeerGroupId",
        skip_serializing_if = "Option::is_none"
    )]
    pub previous_peer_group_id: Option<String>,
    #[serde(
        rename = "previousPeerSetHash",
        skip_serializing_if = "Option::is_none"
    )]
    pub previous_peer_set_hash: Option<String>,
    #[serde(rename = "intendedPeerSetHash")]
    pub intended_peer_set_hash: String,
    #[serde(rename = "reversalCommand")]
    pub reversal_command: String,
    #[serde(rename = "summaryHash")]
    pub summary_hash: String,
    #[serde(rename = "materializationOutcome")]
    pub materialization_outcome: String,
}

/// camelCase mirror of [`IntendedPeer`] for stable on-wire JSON. We keep the
/// snake_case Rust struct for ergonomic Rust callers and convert at the
/// serialization boundary.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct IntendedPeerJson {
    #[serde(rename = "nodeKey")]
    pub node_key: String,
    #[serde(rename = "tailscaleIp")]
    pub tailscale_ip: String,
    #[serde(rename = "magicDnsName", skip_serializing_if = "Option::is_none")]
    pub magic_dns_name: Option<String>,
    pub hostname: String,
    #[serde(rename = "eeProtocolVersion")]
    pub ee_protocol_version: String,
    #[serde(rename = "discoveryPolicyDecision")]
    pub discovery_policy_decision: String,
}

/// camelCase mirror of [`IntendedLanePolicy`].
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct IntendedLanePolicyJson {
    pub metadata: String,
    pub body: String,
    pub embedding: String,
    #[serde(rename = "graphLink")]
    pub graph_link: String,
    #[serde(rename = "revisionNotice")]
    pub revision_notice: String,
    #[serde(rename = "curationSignal")]
    pub curation_signal: String,
}

/// Error returned by [`emit_safety_snapshot_audit`].
///
/// SRR6.46.3 must convert this into an `auto_enrollment_audit_failed`
/// (critical) degraded code and abort without touching the peer-group table.
#[derive(Debug)]
pub enum SafetySnapshotError {
    Serialize(serde_json::Error),
    AuditWrite(DomainError),
}

impl std::fmt::Display for SafetySnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialize(error) => {
                write!(f, "failed to serialize auto-enrollment summary: {error}")
            }
            Self::AuditWrite(error) => {
                write!(f, "failed to write auto-enrollment audit row: {error}")
            }
        }
    }
}

impl std::error::Error for SafetySnapshotError {}

/// Compute the canonical [`AutoEnrollmentSummary`] payload from the input.
///
/// Derives:
///
/// - `intendedPeerSetHash`: `blake3` over a canonical join of the sorted
///   peer-id list with the canonical-JSON lane policy. Stable across
///   peer-list reordering and JSON field-order drift.
/// - `reversalCommand`: literal copy-paste-safe shell line referencing the
///   absolute workspace path.
/// - `summaryHash`: `blake3` over the canonical-JSON of every field except
///   `summaryHash` itself.
///
/// Always computes the same output for the same input (deterministic).
///
/// Panics: never. All field values are pre-validated by callers (SRR6.46.3
/// composes only well-formed inputs); even if a hostname contains weird
/// characters, the helper only hashes bytes, not parses them.
#[must_use]
pub fn compute_summary(input: &AutoEnrollmentSummaryInput<'_>) -> AutoEnrollmentSummary {
    let intended_peers_json: Vec<IntendedPeerJson> = sorted_peer_jsons(&input.intended_peers);
    let intended_lane_policy_json = lane_policy_to_json(input.intended_lane_policy);
    let intended_peer_set_hash =
        compute_intended_peer_set_hash(&intended_peers_json, &intended_lane_policy_json);
    let reversal_command = build_reversal_command(input.workspace_path);

    let mut summary = AutoEnrollmentSummary {
        schema: AUTO_ENROLLMENT_SUMMARY_SCHEMA_V1,
        workspace_id: input.workspace_id.to_owned(),
        tailnet_id: input.tailnet_id.to_owned(),
        tailnet_display_name: input.tailnet_display_name.map(str::to_owned),
        intended_peers: intended_peers_json,
        intended_lane_policy: intended_lane_policy_json,
        trigger_reason: input.trigger_reason.as_str().to_owned(),
        previous_peer_group_id: input.previous_peer_group_id.map(str::to_owned),
        previous_peer_set_hash: input.previous_peer_set_hash.map(str::to_owned),
        intended_peer_set_hash,
        reversal_command,
        summary_hash: String::new(),
        materialization_outcome: input.materialization_outcome.as_str().to_owned(),
    };
    summary.summary_hash = compute_summary_hash(&summary);
    summary
}

/// Emit the safety-snapshot audit row to the existing `ee_audit` chain.
///
/// Returns the generated audit row id on success. The audit row's `details`
/// column carries the canonical JSON of `summary`; the `action` is
/// `audit_actions::MESH_AUTO_ENROLLMENT_INTENDED` so the chain-continuity
/// helpers and the `ee audit timeline --event-type` filter both find it.
///
/// SRR6.46.3 callers MUST abort with `auto_enrollment_audit_failed` on any
/// `Err(_)` return value and MUST NOT touch the peer-group table — that is
/// the load-bearing fail-closed safety property.
pub fn emit_safety_snapshot_audit(
    connection: &DbConnection,
    summary: &AutoEnrollmentSummary,
    actor: Option<&str>,
    target_id: Option<&str>,
) -> Result<String, SafetySnapshotError> {
    let details_json = serde_json::to_string(summary).map_err(SafetySnapshotError::Serialize)?;
    let audit_id = generate_audit_id();
    let input = CreateAuditInput {
        workspace_id: Some(summary.workspace_id.clone()),
        actor: actor.map(str::to_owned),
        action: audit_actions::MESH_AUTO_ENROLLMENT_INTENDED.to_owned(),
        target_type: Some("mesh".to_owned()),
        target_id: target_id.map(str::to_owned),
        details: Some(details_json),
    };
    connection
        .insert_audit(&audit_id, &input)
        .map_err(|error| SafetySnapshotError::AuditWrite(domain_error_from(error)))?;
    Ok(audit_id)
}

/// Back-fill the `materializationOutcome` on an existing safety-snapshot
/// audit row. SRR6.46.3 calls this once it knows whether the peer-group
/// write succeeded, was rolled back, was dry-run, or was audit-only.
///
/// The DB-level audit row is immutable for chain integrity — this function
/// writes a SECOND audit row of type
/// `mesh.auto_enrollment_outcome_recorded` that references the original
/// safety-snapshot `audit_id` via the `details.previousAuditId` field. The
/// chain-continuity verifier joins the two via `prev_chain_hash` and a
/// caller that wants the latest outcome reads the most-recent outcome row.
///
/// This preserves "audit row is immutable" while keeping the audit-row
/// surface easy to query: `ee audit timeline --event-type
/// mesh.auto_enrollment_outcome_recorded` shows every back-fill in order.
pub fn update_materialization_outcome(
    connection: &DbConnection,
    previous_audit_id: &str,
    workspace_id: &str,
    outcome: MaterializationOutcome,
    actor: Option<&str>,
    target_id: Option<&str>,
) -> Result<String, SafetySnapshotError> {
    let details_obj = serde_json::json!({
        "schema": "ee.mesh.auto_enrollment_outcome.v1",
        "previousAuditId": previous_audit_id,
        "materializationOutcome": outcome.as_str(),
    });
    let details_json =
        serde_json::to_string(&details_obj).map_err(SafetySnapshotError::Serialize)?;
    let audit_id = generate_audit_id();
    let input = CreateAuditInput {
        workspace_id: Some(workspace_id.to_owned()),
        actor: actor.map(str::to_owned),
        action: audit_actions::MESH_AUTO_ENROLLMENT_OUTCOME_RECORDED.to_owned(),
        target_type: Some("mesh".to_owned()),
        target_id: target_id.map(str::to_owned),
        details: Some(details_json),
    };
    connection
        .insert_audit(&audit_id, &input)
        .map_err(|error| SafetySnapshotError::AuditWrite(domain_error_from(error)))?;
    Ok(audit_id)
}

// ---------- private helpers -------------------------------------------------

fn sorted_peer_jsons(peers: &[IntendedPeer]) -> Vec<IntendedPeerJson> {
    let mut out: Vec<IntendedPeerJson> = peers
        .iter()
        .map(|peer| IntendedPeerJson {
            node_key: peer.node_key.clone(),
            tailscale_ip: peer.tailscale_ip.clone(),
            magic_dns_name: peer.magic_dns_name.clone(),
            hostname: peer.hostname.clone(),
            ee_protocol_version: peer.ee_protocol_version.clone(),
            discovery_policy_decision: peer.discovery_policy_decision.as_str().to_owned(),
        })
        .collect();
    out.sort_by(|a, b| a.node_key.cmp(&b.node_key));
    out
}

fn lane_policy_to_json(policy: IntendedLanePolicy) -> IntendedLanePolicyJson {
    IntendedLanePolicyJson {
        metadata: policy.metadata.as_str().to_owned(),
        body: policy.body.as_str().to_owned(),
        embedding: policy.embedding.as_str().to_owned(),
        graph_link: policy.graph_link.as_str().to_owned(),
        revision_notice: policy.revision_notice.as_str().to_owned(),
        curation_signal: policy.curation_signal.as_str().to_owned(),
    }
}

fn compute_intended_peer_set_hash(
    peers: &[IntendedPeerJson],
    lane_policy: &IntendedLanePolicyJson,
) -> String {
    // Hash over a canonical join of:
    //   - sorted nodeKey list, one per line
    //   - separator "\n--lane-policy--\n"
    //   - canonical-JSON lane policy
    // This is intentionally narrower than `summaryHash`: only the
    // peer-identity + lane-policy content. Two summaries with same peer
    // set + lane policy share the same `intendedPeerSetHash` even when the
    // tailnet metadata or trigger reason differs. That's what SRR6.46.3
    // uses for the idempotence check.
    let mut hasher = blake3::Hasher::new();
    for peer in peers {
        hasher.update(peer.node_key.as_bytes());
        hasher.update(b"\n");
    }
    hasher.update(b"--lane-policy--\n");
    let lane_canonical = canonical_json_string(
        &serde_json::to_value(lane_policy).unwrap_or(serde_json::Value::Null),
    );
    hasher.update(lane_canonical.as_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn build_reversal_command(workspace_path: &str) -> String {
    // The literal command an operator can copy-paste. The workspace path is
    // double-quoted to survive spaces; an operator on a path containing
    // double quotes is already in trouble for many other reasons. The
    // single-line shape is intentional for clipboard friendliness.
    format!("ee mesh disable --workspace \"{workspace_path}\"")
}

fn compute_summary_hash(summary: &AutoEnrollmentSummary) -> String {
    // Hash over the canonical JSON of every field EXCEPT summaryHash. We
    // clone the summary, clear the hash, canonicalize, and hash.
    let mut clone = summary.clone();
    clone.summary_hash.clear();
    let value = serde_json::to_value(&clone).unwrap_or(serde_json::Value::Null);
    let canonical = canonical_json_string(&value);
    format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex())
}

fn canonical_json_string(value: &serde_json::Value) -> String {
    let canonical = canonicalize_json_value(value);
    serde_json::to_string(&canonical).unwrap_or_else(|_| "null".to_owned())
}

fn canonicalize_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize_json_value(v)))
                .collect();
            let mut out = serde_json::Map::new();
            for (k, v) in sorted {
                out.insert(k, v);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize_json_value).collect())
        }
        other => other.clone(),
    }
}

fn domain_error_from<E: std::fmt::Display>(error: E) -> DomainError {
    DomainError::Storage {
        message: format!("audit insert failed: {error}"),
        repair: Some(
            "Inspect the audit chain (ee audit verify --json) and the workspace DB before retrying auto-enrollment."
                .to_owned(),
        ),
    }
}

// ============================================================================
// Inline unit tests (AGENTS.md L300-302 / bd-3usjw.62 Rule 7)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_input() -> AutoEnrollmentSummaryInput<'static> {
        AutoEnrollmentSummaryInput {
            workspace_id: "wsp_test_workspace_aaaaaa",
            workspace_path: "/Users/test/projects/eidetic_engine_cli",
            tailnet_id: "tn_test_tailnet_id",
            tailnet_display_name: Some("test-tailnet"),
            intended_peers: vec![
                IntendedPeer {
                    node_key: "nodekey:bravo".to_owned(),
                    tailscale_ip: "100.64.0.2".to_owned(),
                    magic_dns_name: Some("peer-b.tailnet".to_owned()),
                    hostname: "peer-b".to_owned(),
                    ee_protocol_version: "0.1".to_owned(),
                    discovery_policy_decision: DiscoveryPolicyDecision::ServiceTagMatch,
                },
                IntendedPeer {
                    node_key: "nodekey:alpha".to_owned(),
                    tailscale_ip: "100.64.0.1".to_owned(),
                    magic_dns_name: None,
                    hostname: "peer-a".to_owned(),
                    ee_protocol_version: "0.1".to_owned(),
                    discovery_policy_decision: DiscoveryPolicyDecision::AutoAdmit,
                },
            ],
            intended_lane_policy: IntendedLanePolicy::conservative_default(),
            trigger_reason: TriggerReason::ManualInvoke,
            previous_peer_group_id: None,
            previous_peer_set_hash: None,
            materialization_outcome: MaterializationOutcome::Pending,
        }
    }

    #[test]
    fn safety_snapshot_records_intended_lane_policy_with_default_deny_for_body_embedding_graph_link()
     {
        let summary = compute_summary(&fixture_input());
        assert_eq!(summary.intended_lane_policy.metadata, "allow");
        assert_eq!(summary.intended_lane_policy.body, "deny");
        assert_eq!(summary.intended_lane_policy.embedding, "deny");
        assert_eq!(summary.intended_lane_policy.graph_link, "deny");
        assert_eq!(summary.intended_lane_policy.revision_notice, "allow");
        assert_eq!(summary.intended_lane_policy.curation_signal, "allow");
    }

    #[test]
    fn safety_snapshot_records_reversal_command_with_absolute_workspace_path_substituted() {
        let mut input = fixture_input();
        input.workspace_path = "/data/projects/eidetic_engine_cli";
        let summary = compute_summary(&input);
        assert_eq!(
            summary.reversal_command,
            "ee mesh disable --workspace \"/data/projects/eidetic_engine_cli\""
        );
        // The reversal command must contain the literal substitution so an
        // operator can copy-paste it without further processing.
        assert!(
            summary
                .reversal_command
                .contains("/data/projects/eidetic_engine_cli")
        );
    }

    #[test]
    fn safety_snapshot_sorts_peers_lexicographically_by_node_key() {
        // The fixture intentionally lists "bravo" before "alpha" so this
        // test catches a regression where peer ordering preserves caller
        // input order instead of canonical lexicographic order.
        let summary = compute_summary(&fixture_input());
        let node_keys: Vec<&str> = summary
            .intended_peers
            .iter()
            .map(|p| p.node_key.as_str())
            .collect();
        assert_eq!(node_keys, vec!["nodekey:alpha", "nodekey:bravo"]);
    }

    #[test]
    fn safety_snapshot_summary_hash_is_byte_stable_across_repeat_serialization() {
        let summary_a = compute_summary(&fixture_input());
        let summary_b = compute_summary(&fixture_input());
        assert_eq!(summary_a.summary_hash, summary_b.summary_hash);
        // The hash format must always look like `blake3:<64-hex>` so
        // downstream forensic-correlation code can split on the prefix.
        assert!(summary_a.summary_hash.starts_with("blake3:"));
        let hex = summary_a
            .summary_hash
            .strip_prefix("blake3:")
            .expect("blake3 prefix");
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn safety_snapshot_summary_hash_differs_when_peer_set_changes() {
        let summary_a = compute_summary(&fixture_input());

        let mut modified_input = fixture_input();
        modified_input.intended_peers.push(IntendedPeer {
            node_key: "nodekey:charlie".to_owned(),
            tailscale_ip: "100.64.0.3".to_owned(),
            magic_dns_name: None,
            hostname: "peer-c".to_owned(),
            ee_protocol_version: "0.1".to_owned(),
            discovery_policy_decision: DiscoveryPolicyDecision::Allowlisted,
        });
        let summary_b = compute_summary(&modified_input);
        assert_ne!(summary_a.summary_hash, summary_b.summary_hash);
        assert_ne!(
            summary_a.intended_peer_set_hash,
            summary_b.intended_peer_set_hash
        );
    }

    #[test]
    fn safety_snapshot_summary_hash_excludes_self_referential_field_from_hash() {
        // The summaryHash MUST NOT include the summaryHash field itself
        // (self-referential — would never converge). Verify by hand:
        // 1. compute summary
        // 2. clear summaryHash, recompute canonical-JSON hash
        // 3. assert the recomputed hash matches the original summaryHash
        let summary = compute_summary(&fixture_input());
        let mut clone = summary.clone();
        clone.summary_hash.clear();
        let value = serde_json::to_value(&clone).expect("serialize");
        let canonical = canonical_json_string(&value);
        let recomputed = format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex());
        assert_eq!(summary.summary_hash, recomputed);
    }

    #[test]
    fn safety_snapshot_intended_peer_set_hash_stable_across_peer_input_reorder() {
        let input_a = fixture_input();

        // Reverse the peer input order before passing in; the output hash
        // must NOT depend on caller-input order because compute_summary
        // sorts internally.
        let mut input_b = fixture_input();
        input_b.intended_peers.reverse();

        let summary_a = compute_summary(&input_a);
        let summary_b = compute_summary(&input_b);
        assert_eq!(
            summary_a.intended_peer_set_hash,
            summary_b.intended_peer_set_hash
        );
        assert_eq!(summary_a.summary_hash, summary_b.summary_hash);
    }

    #[test]
    fn safety_snapshot_serialized_json_contains_schema_constant() {
        let summary = compute_summary(&fixture_input());
        let json = serde_json::to_string(&summary).expect("serialize");
        assert!(json.contains("\"ee.mesh.auto_enrollment_summary.v1\""));
        assert!(json.contains("\"workspaceId\":\"wsp_test_workspace_aaaaaa\""));
        assert!(json.contains("\"triggerReason\":\"manual_invoke\""));
        // Conservative default lane policy must appear in JSON exactly as
        // documented in the schema's defaults.
        assert!(json.contains("\"body\":\"deny\""));
        assert!(json.contains("\"embedding\":\"deny\""));
        assert!(json.contains("\"graphLink\":\"deny\""));
    }

    #[test]
    fn safety_snapshot_serializes_dry_run_outcome_when_input_sets_dry_run() {
        let mut input = fixture_input();
        input.trigger_reason = TriggerReason::DryRunPreview;
        input.materialization_outcome = MaterializationOutcome::DryRun;
        let summary = compute_summary(&input);
        assert_eq!(summary.trigger_reason, "dry_run_preview");
        assert_eq!(summary.materialization_outcome, "dry_run");
    }

    #[test]
    fn safety_snapshot_serializes_drift_reconciliation_outcome_when_audit_only() {
        // Idempotent path: peer set unchanged, audit row emitted under
        // drift_reconciliation with audit_only outcome.
        let mut input = fixture_input();
        input.trigger_reason = TriggerReason::DriftReconciliation;
        input.materialization_outcome = MaterializationOutcome::AuditOnly;
        let summary = compute_summary(&input);
        assert_eq!(summary.trigger_reason, "drift_reconciliation");
        assert_eq!(summary.materialization_outcome, "audit_only");
    }

    #[test]
    fn safety_snapshot_records_previous_peer_group_id_when_replacing_existing_materialization() {
        let mut input = fixture_input();
        input.previous_peer_group_id = Some("pg_existing_aaaaaa");
        input.previous_peer_set_hash =
            Some("blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd");
        let summary = compute_summary(&input);
        assert_eq!(
            summary.previous_peer_group_id.as_deref(),
            Some("pg_existing_aaaaaa")
        );
        assert_eq!(
            summary.previous_peer_set_hash.as_deref(),
            Some("blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd")
        );
    }

    #[test]
    fn safety_snapshot_omits_optional_fields_from_json_when_none() {
        let summary = compute_summary(&fixture_input());
        let json = serde_json::to_string(&summary).expect("serialize");
        assert!(!json.contains("\"previousPeerGroupId\""));
        assert!(!json.contains("\"previousPeerSetHash\""));
    }

    #[test]
    fn trigger_reason_round_trips_through_str() {
        for reason in [
            TriggerReason::FirstRun,
            TriggerReason::ManualInvoke,
            TriggerReason::StewardPass,
            TriggerReason::DriftReconciliation,
            TriggerReason::TailnetChangedRefusal,
            TriggerReason::DryRunPreview,
        ] {
            // Round-trip via serde (which uses the snake_case rename) and
            // via the explicit as_str helper to lock both surfaces.
            let serialized = serde_json::to_string(&reason).expect("serialize");
            let deserialized: TriggerReason =
                serde_json::from_str(&serialized).expect("deserialize");
            assert_eq!(reason, deserialized);
            assert!(serialized.contains(reason.as_str()));
        }
    }

    #[test]
    fn materialization_outcome_round_trips_through_str() {
        for outcome in [
            MaterializationOutcome::Pending,
            MaterializationOutcome::Materialized,
            MaterializationOutcome::RolledBack,
            MaterializationOutcome::DryRun,
            MaterializationOutcome::AuditOnly,
        ] {
            let serialized = serde_json::to_string(&outcome).expect("serialize");
            let deserialized: MaterializationOutcome =
                serde_json::from_str(&serialized).expect("deserialize");
            assert_eq!(outcome, deserialized);
            assert!(serialized.contains(outcome.as_str()));
        }
    }

    #[test]
    fn lane_policy_conservative_default_matches_documented_defaults() {
        // The bead description pins these as the defaults — lock here so
        // someone changing the defaults must consciously update both.
        let policy = IntendedLanePolicy::conservative_default();
        assert_eq!(policy.metadata, LaneDecision::Allow);
        assert_eq!(policy.body, LaneDecision::Deny);
        assert_eq!(policy.embedding, LaneDecision::Deny);
        assert_eq!(policy.graph_link, LaneDecision::Deny);
        assert_eq!(policy.revision_notice, LaneDecision::Allow);
        assert_eq!(policy.curation_signal, LaneDecision::Allow);
    }
}
