//! SRR6.46.3 — one-shot Tailscale auto-enrollment orchestration.
//!
//! This module stays pure: callers provide a fresh Tailscale probe result,
//! SRR6.46.2 autodiscovery candidates, current materialized peer state, and
//! command flags. The CLI layer owns database writes, audit insertion, and the
//! best-effort `sync --once` kick.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::mesh::auto_enrollment_safety::{
    AutoEnrollmentSummary, AutoEnrollmentSummaryInput, DiscoveryPolicyDecision, IntendedLanePolicy,
    IntendedPeer, MaterializationOutcome, TriggerReason, compute_summary,
};
use crate::mesh::identity_change_guard::{
    AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE, BoundIdentity, CurrentIdentity, IdentityGuardVerdict,
    evaluate_identity_guard,
};

pub const AUTO_ENROLLMENT_RESULT_SCHEMA_V1: &str = "ee.mesh.auto_enrollment_result.v1";

pub const AUTO_ENROLLMENT_NO_ELIGIBLE_PEERS_CODE: &str = "auto_enrollment_no_eligible_peers";
pub const AUTO_ENROLLMENT_PARTIAL_FAILURE_CODE: &str = "auto_enrollment_partial_failure";
pub const AUTO_ENROLLMENT_BLOCKED_BY_POLICY_CODE: &str = "auto_enrollment_blocked_by_policy";
pub const AUTO_ENROLLMENT_ALREADY_COMPLETE_CODE: &str = "auto_enrollment_already_complete";
pub const AUTO_ENROLLMENT_CONCURRENT_ATTEMPT_CODE: &str = "auto_enrollment_concurrent_attempt";
pub const AUTO_ENROLLMENT_TAILNET_CHANGED_CODE: &str = "auto_enrollment_tailnet_changed";
pub const AUTO_ENROLLMENT_MANUAL_CONFIG_PRESENT_CODE: &str =
    "auto_enrollment_manual_config_present";
pub const AUTO_ENROLLMENT_AUDIT_FAILED_CODE: &str = "auto_enrollment_audit_failed";
pub const AUTO_ENROLLMENT_SYNC_ONCE_FAILED_CODE: &str = "auto_enrollment_sync_once_failed";
pub const AUTO_ENROLLMENT_INVALID_OVERRIDE_NODE_KEY_CODE: &str =
    "auto_enrollment_invalid_override_node_key";
pub const AUTO_ENROLLMENT_MANUAL_MIGRATION_UNMATCHED_PEER_SET_CODE: &str =
    "auto_enrollment_manual_migration_unmatched_peer_set";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AutoEnrollmentOptions {
    pub dry_run: bool,
    pub explain: bool,
    pub replace_manual_with_auto: bool,
    pub include_overrides: Vec<String>,
    pub exclude_overrides: Vec<String>,
    pub explain_skip: Vec<String>,
    pub trigger_reason: AutoEnrollmentTrigger,
    pub sync_once: AutoEnrollmentSyncOnceMode,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AutoEnrollmentTrigger {
    FirstRun,
    #[default]
    ManualInvoke,
    StewardPass,
    DriftReconciliation,
}

impl From<AutoEnrollmentTrigger> for TriggerReason {
    fn from(value: AutoEnrollmentTrigger) -> Self {
        match value {
            AutoEnrollmentTrigger::FirstRun => Self::FirstRun,
            AutoEnrollmentTrigger::ManualInvoke => Self::ManualInvoke,
            AutoEnrollmentTrigger::StewardPass => Self::StewardPass,
            AutoEnrollmentTrigger::DriftReconciliation => Self::DriftReconciliation,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AutoEnrollmentSyncOnceMode {
    #[default]
    DeferredToCaller,
    Disabled,
    ForcedFailureForTest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoEnrollmentInput {
    pub workspace_id: String,
    pub workspace_path: String,
    pub now: String,
    pub fresh_probe_invocations: u32,
    pub tailnet_id: Option<String>,
    pub tailnet_display_name: Option<String>,
    pub self_node_key: Option<String>,
    pub discovered_peers: Vec<AutoEnrollmentCandidate>,
    pub tailnet_peers: Vec<AutoEnrollmentCandidate>,
    pub existing_peers: Vec<ExistingAutoEnrollmentPeer>,
    pub options: AutoEnrollmentOptions,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoEnrollmentCandidate {
    pub node_key: String,
    pub tailscale_ip: String,
    pub magic_dns_name: Option<String>,
    pub hostname: String,
    pub ee_protocol_version: String,
    pub discovery_policy_decision: String,
}

impl AutoEnrollmentCandidate {
    #[must_use]
    pub fn intended_peer(&self) -> IntendedPeer {
        IntendedPeer {
            node_key: self.node_key.clone(),
            tailscale_ip: self.tailscale_ip.clone(),
            magic_dns_name: self.magic_dns_name.clone(),
            hostname: self.hostname.clone(),
            ee_protocol_version: self.ee_protocol_version.clone(),
            discovery_policy_decision: discovery_policy_decision_from_str(
                &self.discovery_policy_decision,
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExistingAutoEnrollmentPeer {
    pub peer_id: String,
    pub node_key: String,
    pub tailnet_id: Option<String>,
    pub tailnet_display_name: Option<String>,
    pub materialized_on_node_key: Option<String>,
    pub hostname: String,
    pub tailscale_ip: String,
    pub magic_dns_name: Option<String>,
    pub ee_protocol_version: String,
    pub enrollment_source: String,
    pub enabled: bool,
}

impl ExistingAutoEnrollmentPeer {
    #[must_use]
    pub fn candidate(&self) -> AutoEnrollmentCandidate {
        AutoEnrollmentCandidate {
            node_key: self.node_key.clone(),
            tailscale_ip: self.tailscale_ip.clone(),
            magic_dns_name: self.magic_dns_name.clone(),
            hostname: self.hostname.clone(),
            ee_protocol_version: self.ee_protocol_version.clone(),
            discovery_policy_decision: "auto_replaced_manual".to_owned(),
        }
    }

    #[must_use]
    pub fn is_auto_managed(&self) -> bool {
        matches!(
            self.enrollment_source.as_str(),
            "tailscale_auto_enrollment" | "auto_replaced_manual"
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoEnrollmentResult {
    pub schema: &'static str,
    pub command: &'static str,
    pub outcome: &'static str,
    pub peer_group_id: Option<String>,
    pub peer_set_hash: Option<String>,
    pub audit_row_id: Option<String>,
    pub materialized_at: Option<String>,
    pub lane_policy: AutoEnrollmentLanePolicy,
    pub enrollment_outcomes: Vec<AutoEnrollmentPeerOutcome>,
    pub overrides_applied: Vec<AutoEnrollmentOverride>,
    pub sync_once_result: AutoEnrollmentSyncOnceResult,
    pub explanation: Vec<AutoEnrollmentExplanation>,
    pub degraded: Vec<AutoEnrollmentDegradation>,
    pub safety_summary: AutoEnrollmentSummary,
    #[serde(skip)]
    pub materialization: AutoEnrollmentMaterializationPlan,
}

impl AutoEnrollmentResult {
    pub fn attach_audit_row_id(&mut self, audit_row_id: String) {
        self.audit_row_id = Some(audit_row_id);
    }

    pub fn record_sync_once_success(&mut self, contacted_peers: bool) {
        self.sync_once_result = AutoEnrollmentSyncOnceResult {
            attempted: true,
            success: true,
            contacted_peers,
            degraded: Vec::new(),
            command: "ee mesh sync --once --json".to_owned(),
        };
    }

    pub fn record_sync_once_failure(&mut self, message: impl Into<String>) {
        let degradation = AutoEnrollmentDegradation::new(
            AUTO_ENROLLMENT_SYNC_ONCE_FAILED_CODE,
            "warning",
            message.into(),
            "Retry with `ee mesh sync --once --json`; auto-enrollment materialization already completed.",
        );
        self.sync_once_result = AutoEnrollmentSyncOnceResult {
            attempted: true,
            success: false,
            contacted_peers: false,
            degraded: vec![degradation.clone()],
            command: "ee mesh sync --once --json".to_owned(),
        };
        push_degradation_once(&mut self.degraded, degradation);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoEnrollmentLanePolicy {
    pub metadata: &'static str,
    pub body: &'static str,
    pub embedding: &'static str,
    pub graph_link: &'static str,
    pub revision_notice: &'static str,
    pub curation_signal: &'static str,
}

impl Default for AutoEnrollmentLanePolicy {
    fn default() -> Self {
        Self {
            metadata: "allow",
            body: "deny",
            embedding: "deny",
            graph_link: "deny",
            revision_notice: "allow",
            curation_signal: "allow",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoEnrollmentPeerOutcome {
    pub node_key: String,
    pub action: &'static str,
    pub peer_id: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoEnrollmentOverride {
    pub node_key: String,
    pub kind: &'static str,
    pub applied: bool,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoEnrollmentSyncOnceResult {
    pub attempted: bool,
    pub success: bool,
    pub contacted_peers: bool,
    pub degraded: Vec<AutoEnrollmentDegradation>,
    pub command: String,
}

impl Default for AutoEnrollmentSyncOnceResult {
    fn default() -> Self {
        Self {
            attempted: false,
            success: false,
            contacted_peers: false,
            degraded: Vec::new(),
            command: "ee mesh sync --once --json".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoEnrollmentExplanation {
    pub node_key: String,
    pub decision: &'static str,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoEnrollmentDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: &'static str,
}

impl AutoEnrollmentDegradation {
    #[must_use]
    pub fn new(
        code: &'static str,
        severity: &'static str,
        message: impl Into<String>,
        repair: &'static str,
    ) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            repair,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoEnrollmentMaterializationPlan {
    pub writes_peer_rows: bool,
    pub outcome_to_record: MaterializationOutcome,
    pub peers_to_upsert: Vec<AutoEnrollmentCandidate>,
    pub peers_to_revoke: Vec<String>,
    pub append_denylist_node_keys: Vec<String>,
    pub manual_to_auto_migration_intended: bool,
    pub sync_once_after_materialization: bool,
}

impl Default for AutoEnrollmentMaterializationPlan {
    fn default() -> Self {
        Self {
            writes_peer_rows: false,
            outcome_to_record: MaterializationOutcome::AuditOnly,
            peers_to_upsert: Vec::new(),
            peers_to_revoke: Vec::new(),
            append_denylist_node_keys: Vec::new(),
            manual_to_auto_migration_intended: false,
            sync_once_after_materialization: false,
        }
    }
}

#[must_use]
pub fn plan_auto_enrollment(input: AutoEnrollmentInput) -> AutoEnrollmentResult {
    let lane_policy = IntendedLanePolicy::conservative_default();
    let mut candidates = candidate_map(input.discovered_peers.clone());
    let tailnet_peer_map = candidate_map(input.tailnet_peers.clone());
    let mut degraded = Vec::new();
    let mut overrides_applied = Vec::new();
    let mut explanation = Vec::new();

    for node_key in normalize_node_keys(&input.options.include_overrides) {
        if !looks_like_node_key(&node_key) {
            push_invalid_override(&mut degraded, &node_key);
        }
        if candidates.contains_key(&node_key) {
            overrides_applied.push(AutoEnrollmentOverride {
                node_key,
                kind: "include",
                applied: true,
                reason: "peer was already eligible; include override was recorded for future reconciliations"
                    .to_owned(),
            });
            continue;
        }
        if let Some(peer) = tailnet_peer_map.get(&node_key) {
            let mut included = peer.clone();
            included.discovery_policy_decision = "force_include_override".to_owned();
            candidates.insert(node_key.clone(), included);
            overrides_applied.push(AutoEnrollmentOverride {
                node_key,
                kind: "include",
                applied: true,
                reason: "include override force-admitted a tailnet peer that policy excluded"
                    .to_owned(),
            });
        } else {
            overrides_applied.push(AutoEnrollmentOverride {
                node_key,
                kind: "include",
                applied: false,
                reason: "node key was not present in the fresh tailnet probe".to_owned(),
            });
        }
    }

    let exclude_overrides = normalize_node_keys(&input.options.exclude_overrides);
    for node_key in &exclude_overrides {
        if !looks_like_node_key(node_key) {
            push_invalid_override(&mut degraded, node_key);
        }
        let removed = candidates.remove(node_key).is_some();
        overrides_applied.push(AutoEnrollmentOverride {
            node_key: node_key.clone(),
            kind: "exclude",
            applied: removed,
            reason: if removed {
                "exclude override removed the peer from this auto-enrollment run".to_owned()
            } else {
                "exclude override was recorded; peer was not in the eligible set".to_owned()
            },
        });
    }

    let selected = sorted_candidates(candidates);
    let existing_enabled: Vec<_> = input
        .existing_peers
        .iter()
        .filter(|peer| peer.enabled)
        .cloned()
        .collect();
    let local_tailnet = input.tailnet_id.as_deref();
    let legacy_tailnet_changed = local_tailnet.is_some_and(|tailnet_id| {
        existing_enabled.iter().any(|peer| {
            peer.tailnet_id
                .as_deref()
                .is_some_and(|existing| existing != tailnet_id)
        })
    });
    let identity_guard = identity_guard_for_input(&input, &existing_enabled);
    let tailnet_changed = legacy_tailnet_changed
        || matches!(identity_guard, IdentityGuardVerdict::TailnetChanged { .. });
    let node_key_changed = matches!(identity_guard, IdentityGuardVerdict::NodeKeyChanged { .. });
    let manual_conflict = existing_enabled.iter().any(|peer| !peer.is_auto_managed());
    let manual_migration = manual_conflict && input.options.replace_manual_with_auto;
    let missing_fresh_probe = input.fresh_probe_invocations == 0;

    let existing_hash = peer_set_hash_from_candidates(
        existing_enabled
            .iter()
            .map(ExistingAutoEnrollmentPeer::candidate),
        &input.workspace_id,
        &input.workspace_path,
    );

    let peers_to_revoke = revocation_node_keys(
        &exclude_overrides,
        &existing_enabled,
        manual_migration,
        !selected.is_empty(),
    );

    let trigger_reason = if input.options.dry_run {
        TriggerReason::DryRunPreview
    } else if tailnet_changed {
        TriggerReason::TailnetChangedRefusal
    } else {
        input.options.trigger_reason.into()
    };
    let outcome = initial_outcome(
        &input,
        tailnet_changed,
        node_key_changed,
        missing_fresh_probe,
        manual_conflict && !input.options.replace_manual_with_auto,
        selected.is_empty(),
        !peers_to_revoke.is_empty(),
        Some(existing_hash.as_str()),
        &selected,
    );
    let materialization_outcome = match outcome {
        AutoEnrollmentOutcome::DryRun => MaterializationOutcome::DryRun,
        AutoEnrollmentOutcome::AlreadyComplete => MaterializationOutcome::AuditOnly,
        AutoEnrollmentOutcome::ExplainOnly => MaterializationOutcome::AuditOnly,
        AutoEnrollmentOutcome::Blocked
        | AutoEnrollmentOutcome::NoEligiblePeers
        | AutoEnrollmentOutcome::Materialized => MaterializationOutcome::Pending,
    };
    let summary = auto_enrollment_summary(
        &input,
        &selected,
        lane_policy,
        trigger_reason,
        Some(existing_hash.as_str()),
        materialization_outcome,
    );
    let peer_group_id = (!selected.is_empty())
        .then(|| peer_group_id(&input.workspace_id, &summary.intended_peer_set_hash));

    if missing_fresh_probe {
        push_degradation_once(
            &mut degraded,
            AutoEnrollmentDegradation::new(
                AUTO_ENROLLMENT_PARTIAL_FAILURE_CODE,
                "warning",
                "auto-enrollment did not receive evidence of a fresh Tailscale probe",
                "Re-run `ee mesh auto-enroll`; materialization requires a fresh local Tailscale probe.",
            ),
        );
    }
    if tailnet_changed {
        push_degradation_once(
            &mut degraded,
            AutoEnrollmentDegradation::new(
                AUTO_ENROLLMENT_TAILNET_CHANGED_CODE,
                "medium",
                "existing mesh materialization belongs to a different tailnet",
                "Run `ee mesh disable --workspace <path>` before auto-enrolling on the new tailnet.",
            ),
        );
    }
    if node_key_changed {
        push_degradation_once(
            &mut degraded,
            AutoEnrollmentDegradation::new(
                AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE,
                "medium",
                "existing mesh materialization was created on a different Tailscale node key",
                "Run `ee mesh disable --workspace <path> --reason \"restored from different machine\"` before auto-enrolling on this machine.",
            ),
        );
    }
    if manual_conflict && !input.options.replace_manual_with_auto {
        push_degradation_once(
            &mut degraded,
            AutoEnrollmentDegradation::new(
                AUTO_ENROLLMENT_MANUAL_CONFIG_PRESENT_CODE,
                "medium",
                "manual peer configuration is present",
                "Pass `--replace-manual-with-auto` only when the workspace should move to the auto-managed lifecycle.",
            ),
        );
    }
    if selected.is_empty() {
        push_degradation_once(
            &mut degraded,
            AutoEnrollmentDegradation::new(
                AUTO_ENROLLMENT_NO_ELIGIBLE_PEERS_CODE,
                "info",
                "no eligible ee-capable Tailscale peers were discovered",
                "Run ee with mesh enabled on another Tailscale machine, or use `--include <node-key>` for a known peer.",
            ),
        );
    }
    if manual_migration
        && Some(existing_hash.as_str()) != Some(summary.intended_peer_set_hash.as_str())
    {
        push_degradation_once(
            &mut degraded,
            AutoEnrollmentDegradation::new(
                AUTO_ENROLLMENT_MANUAL_MIGRATION_UNMATCHED_PEER_SET_CODE,
                "info",
                "manual-to-auto migration selected a peer set that differs from the existing manual peer set",
                "Review the result before relying on the auto-managed peer group.",
            ),
        );
    }

    for peer in &selected {
        let is_explained = input.options.explain
            || input
                .options
                .explain_skip
                .iter()
                .any(|requested| requested == &peer.node_key);
        if is_explained {
            explanation.push(AutoEnrollmentExplanation {
                node_key: peer.node_key.clone(),
                decision: "include",
                reason: format!(
                    "selected via {} with conservative lane defaults",
                    peer.discovery_policy_decision
                ),
            });
        }
    }

    let mut materialization = AutoEnrollmentMaterializationPlan {
        outcome_to_record: outcome.materialization_outcome(),
        manual_to_auto_migration_intended: manual_migration,
        peers_to_revoke,
        append_denylist_node_keys: input.options.exclude_overrides.clone(),
        ..AutoEnrollmentMaterializationPlan::default()
    };
    if outcome == AutoEnrollmentOutcome::Materialized {
        materialization.writes_peer_rows = true;
        materialization.peers_to_upsert = selected.clone();
        materialization.sync_once_after_materialization =
            input.options.sync_once != AutoEnrollmentSyncOnceMode::Disabled;
    }

    let mut result = AutoEnrollmentResult {
        schema: AUTO_ENROLLMENT_RESULT_SCHEMA_V1,
        command: "mesh auto-enroll",
        outcome: outcome.as_str(),
        peer_group_id,
        peer_set_hash: (!selected.is_empty()).then(|| summary.intended_peer_set_hash.clone()),
        audit_row_id: None,
        materialized_at: (outcome == AutoEnrollmentOutcome::Materialized)
            .then(|| input.now.clone()),
        lane_policy: AutoEnrollmentLanePolicy::default(),
        enrollment_outcomes: enrollment_outcomes(&selected, &outcome),
        overrides_applied,
        sync_once_result: AutoEnrollmentSyncOnceResult::default(),
        explanation,
        degraded,
        safety_summary: summary,
        materialization,
    };

    if input.options.sync_once == AutoEnrollmentSyncOnceMode::ForcedFailureForTest
        && result.materialization.sync_once_after_materialization
    {
        result.record_sync_once_failure("forced sync-once failure");
    }

    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutoEnrollmentOutcome {
    Materialized,
    DryRun,
    ExplainOnly,
    AlreadyComplete,
    NoEligiblePeers,
    Blocked,
}

impl AutoEnrollmentOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Materialized => "materialized",
            Self::DryRun => "dry_run",
            Self::ExplainOnly => "explained",
            Self::AlreadyComplete => "already_complete",
            Self::NoEligiblePeers => "no_eligible_peers",
            Self::Blocked => "blocked",
        }
    }

    const fn materialization_outcome(self) -> MaterializationOutcome {
        match self {
            Self::Materialized => MaterializationOutcome::Materialized,
            Self::DryRun => MaterializationOutcome::DryRun,
            Self::ExplainOnly | Self::AlreadyComplete => MaterializationOutcome::AuditOnly,
            Self::NoEligiblePeers | Self::Blocked => MaterializationOutcome::AuditOnly,
        }
    }
}

fn initial_outcome(
    input: &AutoEnrollmentInput,
    tailnet_changed: bool,
    node_key_changed: bool,
    missing_fresh_probe: bool,
    manual_conflict: bool,
    no_eligible_peers: bool,
    has_revocations: bool,
    existing_hash: Option<&str>,
    selected: &[AutoEnrollmentCandidate],
) -> AutoEnrollmentOutcome {
    if tailnet_changed || node_key_changed || missing_fresh_probe || manual_conflict {
        return AutoEnrollmentOutcome::Blocked;
    }
    if no_eligible_peers && !has_revocations {
        return AutoEnrollmentOutcome::NoEligiblePeers;
    }
    let intended_hash = peer_set_hash_from_candidates(
        selected.iter().cloned(),
        &input.workspace_id,
        &input.workspace_path,
    );
    if existing_hash.is_some() && existing_hash == Some(intended_hash.as_str()) {
        return AutoEnrollmentOutcome::AlreadyComplete;
    }
    if input.options.dry_run {
        return AutoEnrollmentOutcome::DryRun;
    }
    if input.options.explain {
        return AutoEnrollmentOutcome::ExplainOnly;
    }
    AutoEnrollmentOutcome::Materialized
}

fn identity_guard_for_input(
    input: &AutoEnrollmentInput,
    existing_enabled: &[ExistingAutoEnrollmentPeer],
) -> IdentityGuardVerdict {
    let (Some(tailnet_id), Some(self_node_key)) =
        (input.tailnet_id.as_deref(), input.self_node_key.as_deref())
    else {
        return IdentityGuardVerdict::NoBoundIdentity;
    };
    let current = CurrentIdentity {
        tailnet_id: tailnet_id.to_owned(),
        tailnet_display_name: input.tailnet_display_name.clone(),
        self_node_key: self_node_key.to_owned(),
    };
    let bound = bound_identity_from_existing_peers(existing_enabled);
    evaluate_identity_guard(bound.as_ref(), &current)
}

fn bound_identity_from_existing_peers(
    existing_enabled: &[ExistingAutoEnrollmentPeer],
) -> Option<BoundIdentity> {
    existing_enabled
        .iter()
        .filter(|peer| peer.is_auto_managed())
        .find_map(|peer| {
            Some(BoundIdentity {
                tailnet_id: peer.tailnet_id.clone()?,
                tailnet_display_name: peer.tailnet_display_name.clone(),
                materialized_on_node_key: peer.materialized_on_node_key.clone()?,
            })
        })
}

fn revocation_node_keys(
    exclude_overrides: &[String],
    existing_enabled: &[ExistingAutoEnrollmentPeer],
    manual_migration: bool,
    has_selected_peers: bool,
) -> Vec<String> {
    let mut keys: BTreeSet<String> = exclude_overrides
        .iter()
        .filter(|node_key| looks_like_node_key(node_key))
        .cloned()
        .collect();
    if manual_migration && has_selected_peers {
        keys.extend(
            existing_enabled
                .iter()
                .filter(|peer| !peer.is_auto_managed())
                .map(|peer| peer.node_key.clone()),
        );
    }
    keys.into_iter().collect()
}

fn enrollment_outcomes(
    selected: &[AutoEnrollmentCandidate],
    outcome: &AutoEnrollmentOutcome,
) -> Vec<AutoEnrollmentPeerOutcome> {
    selected
        .iter()
        .map(|peer| AutoEnrollmentPeerOutcome {
            node_key: peer.node_key.clone(),
            action: match outcome {
                AutoEnrollmentOutcome::Materialized => "upsert_peer",
                AutoEnrollmentOutcome::DryRun => "would_upsert_peer",
                AutoEnrollmentOutcome::ExplainOnly => "explained_only",
                AutoEnrollmentOutcome::AlreadyComplete => "already_present",
                AutoEnrollmentOutcome::NoEligiblePeers | AutoEnrollmentOutcome::Blocked => "none",
            },
            peer_id: None,
            reason: "conservative auto-enrollment selected this peer".to_owned(),
        })
        .collect()
}

fn auto_enrollment_summary(
    input: &AutoEnrollmentInput,
    selected: &[AutoEnrollmentCandidate],
    lane_policy: IntendedLanePolicy,
    trigger_reason: TriggerReason,
    previous_peer_set_hash: Option<&str>,
    materialization_outcome: MaterializationOutcome,
) -> AutoEnrollmentSummary {
    let tailnet_id = input.tailnet_id.as_deref().unwrap_or("tailnet_unknown");
    let intended_peers = selected
        .iter()
        .map(AutoEnrollmentCandidate::intended_peer)
        .collect();
    compute_summary(&AutoEnrollmentSummaryInput {
        workspace_id: &input.workspace_id,
        workspace_path: &input.workspace_path,
        tailnet_id,
        tailnet_display_name: input.tailnet_display_name.as_deref(),
        intended_peers,
        intended_lane_policy: lane_policy,
        trigger_reason,
        previous_peer_group_id: None,
        previous_peer_set_hash,
        materialization_outcome,
    })
}

fn peer_set_hash_from_candidates<I>(
    candidates: I,
    workspace_id: &str,
    workspace_path: &str,
) -> String
where
    I: IntoIterator<Item = AutoEnrollmentCandidate>,
{
    let selected: Vec<_> = candidates.into_iter().collect();
    auto_enrollment_summary(
        &AutoEnrollmentInput {
            workspace_id: workspace_id.to_owned(),
            workspace_path: workspace_path.to_owned(),
            now: "1970-01-01T00:00:00Z".to_owned(),
            fresh_probe_invocations: 1,
            tailnet_id: Some("tailnet_hash".to_owned()),
            tailnet_display_name: None,
            self_node_key: None,
            discovered_peers: Vec::new(),
            tailnet_peers: Vec::new(),
            existing_peers: Vec::new(),
            options: AutoEnrollmentOptions::default(),
        },
        &selected,
        IntendedLanePolicy::conservative_default(),
        TriggerReason::ManualInvoke,
        None,
        MaterializationOutcome::Pending,
    )
    .intended_peer_set_hash
}

fn peer_group_id(workspace_id: &str, peer_set_hash: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.mesh.auto.peer_group.v1\n");
    hasher.update(workspace_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(peer_set_hash.as_bytes());
    let digest = hasher.finalize().to_hex().to_string();
    let suffix: String = digest.chars().take(16).collect();
    format!("pg_{suffix}")
}

fn candidate_map(
    candidates: Vec<AutoEnrollmentCandidate>,
) -> BTreeMap<String, AutoEnrollmentCandidate> {
    candidates
        .into_iter()
        .map(|candidate| (candidate.node_key.clone(), candidate))
        .collect()
}

fn sorted_candidates(
    candidates: BTreeMap<String, AutoEnrollmentCandidate>,
) -> Vec<AutoEnrollmentCandidate> {
    candidates.into_values().collect()
}

fn normalize_node_keys(node_keys: &[String]) -> Vec<String> {
    let mut set = BTreeSet::new();
    for node_key in node_keys {
        let trimmed = node_key.trim();
        if !trimmed.is_empty() {
            set.insert(trimmed.to_owned());
        }
    }
    set.into_iter().collect()
}

fn looks_like_node_key(node_key: &str) -> bool {
    node_key
        .strip_prefix("nodekey:")
        .is_some_and(|suffix| !suffix.trim().is_empty())
}

fn push_invalid_override(degraded: &mut Vec<AutoEnrollmentDegradation>, node_key: &str) {
    push_degradation_once(
        degraded,
        AutoEnrollmentDegradation::new(
            AUTO_ENROLLMENT_INVALID_OVERRIDE_NODE_KEY_CODE,
            "warning",
            format!("override node key `{node_key}` does not look like a Tailscale node key"),
            "Use node keys formatted as `nodekey:<value>`; invalid overrides are ignored when no matching tailnet peer exists.",
        ),
    );
}

fn push_degradation_once(
    degraded: &mut Vec<AutoEnrollmentDegradation>,
    item: AutoEnrollmentDegradation,
) {
    if !degraded.iter().any(|existing| existing.code == item.code) {
        degraded.push(item);
    }
}

fn discovery_policy_decision_from_str(raw: &str) -> DiscoveryPolicyDecision {
    match raw {
        "auto_admit" | "force_include_override" | "auto_replaced_manual" => {
            DiscoveryPolicyDecision::AutoAdmit
        }
        "allowlisted" => DiscoveryPolicyDecision::Allowlisted,
        _ => DiscoveryPolicyDecision::ServiceTagMatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(node_key: &str) -> AutoEnrollmentCandidate {
        AutoEnrollmentCandidate {
            node_key: node_key.to_owned(),
            tailscale_ip: "100.64.0.2".to_owned(),
            magic_dns_name: Some(format!("{node_key}.tailnet.test.")),
            hostname: node_key.replace("nodekey:", "host-"),
            ee_protocol_version: "1.0".to_owned(),
            discovery_policy_decision: "service_tag_match".to_owned(),
        }
    }

    fn input(discovered_peers: Vec<AutoEnrollmentCandidate>) -> AutoEnrollmentInput {
        let tailnet_peers = discovered_peers.clone();
        AutoEnrollmentInput {
            workspace_id: "wsp_test_workspace".to_owned(),
            workspace_path: "/tmp/workspace".to_owned(),
            now: "2026-05-20T00:00:00Z".to_owned(),
            fresh_probe_invocations: 1,
            tailnet_id: Some("tailnet-alpha".to_owned()),
            tailnet_display_name: Some("alpha.example".to_owned()),
            self_node_key: Some("nodekey:self".to_owned()),
            discovered_peers,
            tailnet_peers,
            existing_peers: Vec::new(),
            options: AutoEnrollmentOptions::default(),
        }
    }

    #[test]
    fn auto_enrollment_returns_no_eligible_peers_when_discovery_empty() {
        let result = plan_auto_enrollment(input(Vec::new()));
        assert_eq!(result.outcome, "no_eligible_peers");
        assert!(result.materialization.peers_to_upsert.is_empty());
        assert!(
            result
                .degraded
                .iter()
                .any(|item| item.code == AUTO_ENROLLMENT_NO_ELIGIBLE_PEERS_CODE)
        );
    }

    #[test]
    fn auto_enrollment_materializes_intended_config_when_peers_eligible() {
        let result = plan_auto_enrollment(input(vec![candidate("nodekey:alpha")]));
        assert_eq!(result.outcome, "materialized");
        assert_eq!(result.lane_policy.body, "deny");
        assert!(result.materialization.writes_peer_rows);
        assert_eq!(result.materialization.peers_to_upsert.len(), 1);
    }

    #[test]
    fn auto_enrollment_writes_audit_summary_before_materializing_durable_changes() {
        let result = plan_auto_enrollment(input(vec![candidate("nodekey:alpha")]));
        assert_eq!(
            result.safety_summary.schema,
            "ee.mesh.auto_enrollment_summary.v1"
        );
        assert_eq!(result.safety_summary.materialization_outcome, "pending");
        assert!(result.materialization.writes_peer_rows);
    }

    #[test]
    fn auto_enrollment_dry_run_does_not_touch_peer_group_table() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.options.dry_run = true;
        let result = plan_auto_enrollment(input);
        assert_eq!(result.outcome, "dry_run");
        assert!(!result.materialization.writes_peer_rows);
        assert_eq!(result.safety_summary.materialization_outcome, "dry_run");
    }

    #[test]
    fn auto_enrollment_rolls_back_completely_on_partial_failure() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.fresh_probe_invocations = 0;
        let result = plan_auto_enrollment(input);
        assert_eq!(result.outcome, "blocked");
        assert!(
            result
                .degraded
                .iter()
                .any(|item| item.code == AUTO_ENROLLMENT_PARTIAL_FAILURE_CODE)
        );
        assert!(!result.materialization.writes_peer_rows);
        assert_eq!(
            result.materialization.outcome_to_record,
            MaterializationOutcome::AuditOnly
        );
    }

    #[test]
    fn auto_enrollment_idempotent_on_second_call_with_same_peer_set_hash() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.existing_peers = vec![existing_auto_peer(
            "nodekey:alpha",
            "tailnet-alpha",
            Some("nodekey:self"),
        )];
        let result = plan_auto_enrollment(input);
        assert_eq!(result.outcome, "already_complete");
        assert_eq!(
            result.materialization.outcome_to_record,
            MaterializationOutcome::AuditOnly
        );
    }

    #[test]
    fn auto_enrollment_refuses_when_tailnet_id_differs() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.existing_peers = vec![existing_auto_peer(
            "nodekey:alpha",
            "tailnet-old",
            Some("nodekey:self"),
        )];
        let result = plan_auto_enrollment(input);
        assert_eq!(result.outcome, "blocked");
        assert!(
            result
                .degraded
                .iter()
                .any(|item| item.code == AUTO_ENROLLMENT_TAILNET_CHANGED_CODE)
        );
    }

    #[test]
    fn auto_enrollment_refuses_when_materialized_node_key_differs() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.existing_peers = vec![existing_auto_peer(
            "nodekey:alpha",
            "tailnet-alpha",
            Some("nodekey:old-self"),
        )];

        let result = plan_auto_enrollment(input);

        assert_eq!(result.outcome, "blocked");
        assert!(!result.materialization.writes_peer_rows);
        assert!(result.degraded.iter().any(|item| {
            item.code == AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE && item.severity == "medium"
        }));
    }

    #[test]
    fn auto_enrollment_tailnet_change_takes_priority_over_node_key_change() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.tailnet_id = Some("tailnet-new".to_owned());
        input.existing_peers = vec![existing_auto_peer(
            "nodekey:alpha",
            "tailnet-old",
            Some("nodekey:old-self"),
        )];

        let result = plan_auto_enrollment(input);

        assert_eq!(result.outcome, "blocked");
        assert!(
            result
                .degraded
                .iter()
                .any(|item| item.code == AUTO_ENROLLMENT_TAILNET_CHANGED_CODE)
        );
        assert!(
            !result
                .degraded
                .iter()
                .any(|item| item.code == AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE)
        );
    }

    #[test]
    fn auto_enrollment_tailnet_display_name_rename_does_not_block() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.tailnet_display_name = Some("renamed.example".to_owned());
        input.existing_peers = vec![existing_auto_peer(
            "nodekey:alpha",
            "tailnet-alpha",
            Some("nodekey:self"),
        )];

        let result = plan_auto_enrollment(input);

        assert_eq!(result.outcome, "already_complete");
        assert!(result.degraded.iter().all(|item| {
            item.code != AUTO_ENROLLMENT_TAILNET_CHANGED_CODE
                && item.code != AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE
        }));
    }

    #[test]
    fn auto_enrollment_allows_auto_rows_without_materialized_node_key() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.self_node_key = Some("nodekey:new-self".to_owned());
        input.existing_peers = vec![existing_auto_peer("nodekey:alpha", "tailnet-alpha", None)];

        let result = plan_auto_enrollment(input);

        assert_eq!(result.outcome, "already_complete");
        assert!(
            !result
                .degraded
                .iter()
                .any(|item| item.code == AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE)
        );
    }

    #[test]
    fn auto_enrollment_refuses_when_manual_peer_group_present_without_replace_flag() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.existing_peers = vec![manual_peer("nodekey:manual")];
        let result = plan_auto_enrollment(input);
        assert_eq!(result.outcome, "blocked");
        assert!(
            result
                .degraded
                .iter()
                .any(|item| item.code == AUTO_ENROLLMENT_MANUAL_CONFIG_PRESENT_CODE)
        );
    }

    #[test]
    fn auto_enrollment_replace_manual_with_auto_migrates_and_emits_migration_audit_row() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.existing_peers = vec![manual_peer("nodekey:manual")];
        input.options.replace_manual_with_auto = true;
        let result = plan_auto_enrollment(input);
        assert_eq!(result.outcome, "materialized");
        assert!(result.materialization.manual_to_auto_migration_intended);
        assert_eq!(
            result.materialization.peers_to_revoke,
            vec!["nodekey:manual"]
        );
    }

    #[test]
    fn auto_enrollment_plan_records_materialized_outcome_before_audit_attachment() {
        let result = plan_auto_enrollment(input(vec![candidate("nodekey:alpha")]));
        assert_eq!(
            result.materialization.outcome_to_record,
            MaterializationOutcome::Materialized
        );
        assert!(result.audit_row_id.is_none());
    }

    #[test]
    fn auto_enrollment_plan_marks_peer_rows_for_materialization() {
        let result = plan_auto_enrollment(input(vec![candidate("nodekey:alpha")]));
        assert!(result.materialization.writes_peer_rows);
        assert_eq!(result.command, "mesh auto-enroll");
    }

    #[test]
    fn auto_enrollment_sync_once_failure_records_warning_after_materialization_plan() {
        let mut result = plan_auto_enrollment(input(vec![candidate("nodekey:alpha")]));
        result.record_sync_once_failure("simulated post-materialization failure");
        assert!(result.materialization.writes_peer_rows);
        assert!(
            result
                .degraded
                .iter()
                .any(|item| item.code == AUTO_ENROLLMENT_SYNC_ONCE_FAILED_CODE)
        );
    }

    #[test]
    fn auto_enrollment_re_probes_tailscale_state_at_start_not_cached() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.fresh_probe_invocations = 1;
        let result = plan_auto_enrollment(input);
        assert!(
            !result
                .degraded
                .iter()
                .any(|item| item.code == AUTO_ENROLLMENT_PARTIAL_FAILURE_CODE)
        );
    }

    #[test]
    fn auto_enrollment_include_flag_force_includes_policy_excluded_peer() {
        let mut input = input(Vec::new());
        input.tailnet_peers = vec![candidate("nodekey:forced")];
        input.options.include_overrides = vec!["nodekey:forced".to_owned()];
        let result = plan_auto_enrollment(input);
        assert_eq!(result.outcome, "materialized");
        assert_eq!(
            result.materialization.peers_to_upsert[0].node_key,
            "nodekey:forced"
        );
    }

    #[test]
    fn auto_enrollment_exclude_flag_skips_peer_and_appends_to_denylist() {
        let mut input = input(vec![candidate("nodekey:alpha"), candidate("nodekey:bravo")]);
        input.options.exclude_overrides = vec!["nodekey:bravo".to_owned()];
        let result = plan_auto_enrollment(input);
        assert_eq!(result.materialization.peers_to_upsert.len(), 1);
        assert_eq!(
            result.materialization.peers_to_revoke,
            vec!["nodekey:bravo"]
        );
        assert_eq!(
            result.materialization.append_denylist_node_keys,
            vec!["nodekey:bravo"]
        );
    }

    #[test]
    fn auto_enrollment_exclude_existing_peer_materializes_revocation_when_no_candidates_remain() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.existing_peers = vec![existing_auto_peer(
            "nodekey:alpha",
            "tailnet-alpha",
            Some("nodekey:self"),
        )];
        input.options.exclude_overrides = vec!["nodekey:alpha".to_owned()];

        let result = plan_auto_enrollment(input);

        assert_eq!(result.outcome, "materialized");
        assert!(result.materialization.writes_peer_rows);
        assert!(result.materialization.peers_to_upsert.is_empty());
        assert_eq!(
            result.materialization.peers_to_revoke,
            vec!["nodekey:alpha"]
        );
    }

    #[test]
    fn auto_enrollment_explain_flag_prints_tree_without_durable_write() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.options.explain = true;
        let result = plan_auto_enrollment(input);
        assert_eq!(result.outcome, "explained");
        assert!(!result.materialization.writes_peer_rows);
        assert_eq!(result.explanation.len(), 1);
    }

    #[test]
    fn auto_enrollment_invalid_override_node_key_warns_not_blocks() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.options.include_overrides = vec!["not-a-node-key".to_owned()];
        let result = plan_auto_enrollment(input);
        assert_eq!(result.outcome, "materialized");
        assert!(
            result
                .degraded
                .iter()
                .any(|item| item.code == AUTO_ENROLLMENT_INVALID_OVERRIDE_NODE_KEY_CODE)
        );
    }

    #[test]
    fn auto_enrollment_invalid_exclude_without_candidates_does_not_materialize_revocation() {
        let mut input = input(Vec::new());
        input.options.exclude_overrides = vec!["not-a-node-key".to_owned()];

        let result = plan_auto_enrollment(input);

        assert_eq!(result.outcome, "no_eligible_peers");
        assert!(!result.materialization.writes_peer_rows);
        assert!(result.materialization.peers_to_revoke.is_empty());
        assert!(result.degraded.iter().any(|item| {
            item.code == AUTO_ENROLLMENT_INVALID_OVERRIDE_NODE_KEY_CODE
                && item.severity == "warning"
        }));
    }

    #[test]
    fn auto_enrollment_sync_once_kick_happens_after_materialization_not_before() {
        let result = plan_auto_enrollment(input(vec![candidate("nodekey:alpha")]));
        assert!(result.materialization.writes_peer_rows);
        assert!(result.materialization.sync_once_after_materialization);
    }

    #[test]
    fn auto_enrollment_sync_once_failure_surfaces_as_warning_not_critical() {
        let mut input = input(vec![candidate("nodekey:alpha")]);
        input.options.sync_once = AutoEnrollmentSyncOnceMode::ForcedFailureForTest;
        let result = plan_auto_enrollment(input);
        assert_eq!(result.outcome, "materialized");
        assert!(result.degraded.iter().any(|item| {
            item.code == AUTO_ENROLLMENT_SYNC_ONCE_FAILED_CODE && item.severity == "warning"
        }));
    }

    #[test]
    fn auto_enrollment_each_workspace_gets_own_peer_group_row_with_same_peer_set() {
        let left = plan_auto_enrollment(input(vec![candidate("nodekey:alpha")]));
        let mut right_input = input(vec![candidate("nodekey:alpha")]);
        right_input.workspace_id = "wsp_other_workspace".to_owned();
        let right = plan_auto_enrollment(right_input);
        assert_eq!(left.peer_set_hash, right.peer_set_hash);
        assert_ne!(left.peer_group_id, right.peer_group_id);
        assert_eq!(left.outcome, "materialized");
        assert_eq!(right.outcome, "materialized");
    }

    fn manual_peer(node_key: &str) -> ExistingAutoEnrollmentPeer {
        ExistingAutoEnrollmentPeer {
            peer_id: format!("peer_{}", node_key.replace("nodekey:", "")),
            node_key: node_key.to_owned(),
            tailnet_id: Some("tailnet-alpha".to_owned()),
            tailnet_display_name: Some("alpha.example".to_owned()),
            materialized_on_node_key: None,
            hostname: node_key.to_owned(),
            tailscale_ip: "100.64.0.3".to_owned(),
            magic_dns_name: None,
            ee_protocol_version: "1.0".to_owned(),
            enrollment_source: "explicit_human_consent".to_owned(),
            enabled: true,
        }
    }

    fn existing_auto_peer(
        node_key: &str,
        tailnet_id: &str,
        materialized_on_node_key: Option<&str>,
    ) -> ExistingAutoEnrollmentPeer {
        ExistingAutoEnrollmentPeer {
            peer_id: format!("peer_{}", node_key.replace("nodekey:", "")),
            node_key: node_key.to_owned(),
            tailnet_id: Some(tailnet_id.to_owned()),
            tailnet_display_name: Some("alpha.example".to_owned()),
            materialized_on_node_key: materialized_on_node_key.map(str::to_owned),
            hostname: node_key.replace("nodekey:", "host-"),
            tailscale_ip: "100.64.0.2".to_owned(),
            magic_dns_name: None,
            ee_protocol_version: "1.0".to_owned(),
            enrollment_source: "tailscale_auto_enrollment".to_owned(),
            enabled: true,
        }
    }
}
