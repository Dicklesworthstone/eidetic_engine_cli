//! Read-only next-action snapshot for swarm work allocation.
//!
//! This module intentionally builds on the existing `swarm brief` collectors.
//! SWA1 defines the stable input snapshot; later SWA beads can add ranking and
//! reservation suggestions without re-collecting source state.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::Path;

use chrono::Utc;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::beads_integrity::{
    BeadsIntegrityHealth, BeadsIntegrityInputs, BeadsIntegrityReport, compose_integrity_report,
    compose_integrity_report_from_br_doctor_json,
};
use crate::core::environment_attestation::{
    EnvironmentAttestationSourceTestVerdict, EnvironmentAttestationSummary,
    EnvironmentAttestationVerdict,
};
use crate::core::preflight_guard::classify_repair_command_for_preflight;
use crate::core::swarm_brief::{
    SwarmBriefBead, SwarmBriefCollectOptions, SwarmBriefCommandRunner, SwarmBriefCommit,
    SwarmBriefDegradation, SwarmBriefFileReservation, SwarmBriefReport, SwarmBriefSourceKind,
    SwarmBriefThreadSummary, collect_swarm_brief,
};
use crate::core::verify_ledger::{RchVerifyRunView, list_rch_verify_blockers};
use crate::db::DbConnection;

pub const SWARM_NEXT_ACTION_SCHEMA_V1: &str = "ee.swarm_next_action.v1";
pub const SWARM_NEXT_ACTION_REDACTION_STATUS: &str =
    "counts_ids_statuses_paths_redacted_no_mail_body_no_file_content";
pub const SWARM_WORK_PACKET_SCHEMA_V1: &str = "ee.swarm.work_packet.v1";
pub const SWARM_WORK_PACKET_CLAIM_GATE_SCHEMA_V1: &str = "ee.swarm.work_packet.claim_gate.v1";
pub const SWARM_WORK_PACKET_REDACTION_STATUS: &str =
    "counts_ids_statuses_path_patterns_command_templates_no_mail_body_no_file_content";
const EXTERNAL_AGENT_SPACE_ROOT: &str = "/Volumes/USBNVME16TB/temp_agent_space";
const AGENT_MAIL_UNAVAILABLE_CODE: &str = "agent_mail_unavailable";
const AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE: &str = "agent_mail_semantic_readiness_failed";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmNextActionSnapshot {
    pub schema: &'static str,
    pub workspace: String,
    pub redaction_status: &'static str,
    pub inputs: SwarmNextActionInputSummary,
    pub candidates: Vec<SwarmNextActionCandidate>,
    pub stale_work_proposals: Vec<SwarmNextActionStaleWorkProposal>,
    pub coordination: SwarmNextActionCoordinationSummary,
    pub checkout: SwarmNextActionCheckoutSummary,
    pub compile_health: SwarmNextActionCompileHealthSummary,
    pub verification: SwarmNextActionVerificationSummary,
    pub environment: SwarmNextActionEnvironmentSummary,
    pub degraded: Vec<SwarmNextActionDegradation>,
}

impl Serialize for SwarmNextActionSnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("SwarmNextActionSnapshot", 13)?;
        state.serialize_field("schema", &self.schema)?;
        state.serialize_field("workspace", &self.workspace)?;
        state.serialize_field("redactionStatus", &self.redaction_status)?;
        state.serialize_field("inputs", &self.inputs)?;
        state.serialize_field("candidates", &self.candidates)?;
        state.serialize_field("recommendationCards", &self.recommendation_cards())?;
        state.serialize_field("staleWorkProposals", &self.stale_work_proposals)?;
        state.serialize_field("coordination", &self.coordination)?;
        state.serialize_field("checkout", &self.checkout)?;
        state.serialize_field("compileHealth", &self.compile_health)?;
        state.serialize_field("verification", &self.verification)?;
        state.serialize_field("environment", &self.environment)?;
        state.serialize_field("degraded", &self.degraded)?;
        state.end()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionInputSummary {
    pub source_count: usize,
    pub ready_bead_count: usize,
    pub in_progress_bead_count: usize,
    pub blocked_bead_count: usize,
    pub bv_top_pick_count: usize,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionCandidate {
    pub id: String,
    pub title: String,
    pub source: &'static str,
    pub score_milli: Option<u32>,
    pub status: String,
    pub priority: Option<i64>,
    #[serde(skip)]
    pub issue_type: Option<String>,
    pub assignee: Option<String>,
    pub blocked_by: Vec<String>,
    pub blocked_by_compile_health: bool,
    pub action_hint: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionStaleWorkProposal {
    pub bead_id: String,
    pub title: String,
    pub assignee: Option<String>,
    pub decision: &'static str,
    pub confidence: &'static str,
    pub evidence: Vec<String>,
    pub caveats: Vec<String>,
    pub suggested_commands: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionRecommendationCard {
    pub card_id: String,
    pub candidate_id: Option<String>,
    pub candidate_source: &'static str,
    pub candidate_summary: String,
    pub decision: &'static str,
    pub confidence: &'static str,
    pub score_inputs: Vec<SwarmNextActionScoreInput>,
    pub suggested_reservations: Vec<SwarmNextActionSuggestedReservation>,
    pub do_not_take_because: Vec<String>,
    pub overlap: SwarmNextActionOverlapDecision,
    pub proof_obligations: Vec<String>,
    pub evidence_caveats: Vec<String>,
    pub fallback_decision: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionScoreInput {
    pub name: &'static str,
    pub value: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionSuggestedReservation {
    pub path_pattern: String,
    pub exclusive: bool,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionOverlapDecision {
    pub decision: &'static str,
    pub queries: Vec<String>,
    pub matched_existing_beads: Vec<String>,
    pub rejected_duplicate_reason: Option<&'static str>,
    pub selected_relation: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionCoordinationSummary {
    pub active_reservation_count: usize,
    pub reservation_holders: Vec<String>,
    pub unread_inbox_count: u64,
    pub ack_required_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionCheckoutSummary {
    pub dirty_path_count: usize,
    pub dirty_paths: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionCompileHealthSummary {
    pub safe_to_launch_rch: Option<bool>,
    pub blocker_count: usize,
    pub blockers: Vec<SwarmNextActionCompileHealthBlocker>,
    pub recommended_alternative_work: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionCompileHealthBlocker {
    pub path: String,
    pub severity: &'static str,
    pub reason: &'static str,
    pub owner_agent: Option<String>,
    pub owner_pattern: Option<String>,
    pub recent_first_error: Option<SwarmNextActionRecentFirstError>,
    pub affected_command_kinds: Vec<String>,
    pub suggested_next_action: &'static str,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionRecentFirstError {
    pub file: String,
    pub line: Option<u64>,
    pub command_kind: Option<String>,
    pub command: Option<String>,
    pub command_hash: Option<String>,
    pub status: Option<String>,
    pub degraded_codes: Vec<String>,
    #[serde(skip)]
    pub source_state_hash: Option<String>,
    #[serde(skip)]
    pub created_at: Option<String>,
    #[serde(skip)]
    pub error_codes: Vec<String>,
    #[serde(skip)]
    pub remote_required: Option<bool>,
    #[serde(skip)]
    pub local_fallback_refused: bool,
    #[serde(skip)]
    pub retry_after: Option<String>,
    #[serde(skip)]
    pub known_blocker: Option<SwarmWorkPacketKnownBlocker>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmNextActionVerificationSummary {
    pub rch_source_enabled: bool,
    pub remote_only_required: bool,
    pub remote_only_safe: Option<bool>,
    pub healthy_worker_count: Option<u64>,
    pub active_remote_build_count: Option<u64>,
    pub queued_remote_build_count: Option<u64>,
    pub slots_available: Option<u64>,
    pub queue_head_slots_needed: Option<u64>,
    pub active_build_max_age_seconds: Option<u64>,
    pub queue_status: Option<String>,
    pub verifier_evidence: Vec<SwarmNextActionRecentFirstError>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct VerifierSuccessfulProof {
    command_hash: String,
    source_state_hash: String,
    created_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionAdmissionCertificate {
    pub schema: &'static str,
    pub action: &'static str,
    pub rule_id: &'static str,
    pub confidence: &'static str,
    pub service_class: &'static str,
    pub service_time_class: &'static str,
    pub service_time_interval_ms: SwarmNextActionServiceTimeIntervalMs,
    pub queue_risk_class: &'static str,
    pub predictor_coverage: &'static str,
    pub predictor_mode: &'static str,
    pub conservative_reason: Option<&'static str>,
    pub evidence: Vec<String>,
    pub assumptions: Vec<&'static str>,
    pub proof_obligations: Vec<&'static str>,
    pub safety_invariant: &'static str,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionServiceTimeIntervalMs {
    pub lower: u64,
    pub upper: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmNextActionServiceTimeEstimate {
    pub service_time_class: &'static str,
    pub service_time_interval_ms: SwarmNextActionServiceTimeIntervalMs,
    pub queue_risk_class: &'static str,
    pub predictor_coverage: &'static str,
    pub predictor_mode: &'static str,
    pub conservative_reason: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmNextActionServiceTimeEvidenceRecord {
    pub command_family: String,
    pub duration_ms: u64,
    pub queue_wait_ms: u64,
    pub observed_age_seconds: u64,
    pub failure_class: Option<String>,
    pub worker_class: Option<String>,
    pub duplicate_bead_attribution: bool,
}

impl Serialize for SwarmNextActionVerificationSummary {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("SwarmNextActionVerificationSummary", 14)?;
        state.serialize_field("rchSourceEnabled", &self.rch_source_enabled)?;
        state.serialize_field("remoteOnlyRequired", &self.remote_only_required)?;
        state.serialize_field("remoteOnlySafe", &self.remote_only_safe)?;
        state.serialize_field("healthyWorkerCount", &self.healthy_worker_count)?;
        state.serialize_field("activeRemoteBuildCount", &self.active_remote_build_count)?;
        state.serialize_field("queuedRemoteBuildCount", &self.queued_remote_build_count)?;
        state.serialize_field("slotsAvailable", &self.slots_available)?;
        state.serialize_field("queueHeadSlotsNeeded", &self.queue_head_slots_needed)?;
        state.serialize_field(
            "activeBuildMaxAgeSeconds",
            &self.active_build_max_age_seconds,
        )?;
        state.serialize_field("headOfLineBlocked", &self.head_of_line_blocked())?;
        state.serialize_field("queueRecommendation", &self.queue_recommendation())?;
        state.serialize_field("queueStatus", &self.queue_status)?;
        state.serialize_field("queueEvidence", &self.queue_evidence())?;
        state.serialize_field("admissionCertificate", &self.admission_certificate())?;
        state.end()
    }
}

impl SwarmNextActionVerificationSummary {
    #[must_use]
    pub fn head_of_line_blocked(&self) -> Option<bool> {
        let queued = self.queued_remote_build_count?;
        let slots_available = self.slots_available?;
        let queue_head_slots_needed = self.queue_head_slots_needed?;

        Some(queued > 0 && slots_available > 0 && slots_available < queue_head_slots_needed)
    }

    #[must_use]
    pub fn suspected_orphaned_queued_verifier_count(&self) -> Option<u64> {
        let queued = self.queued_remote_build_count?;
        let active = self.active_remote_build_count?;
        if queued == 0 {
            return Some(0);
        }
        let queue_head_can_start = self
            .slots_available
            .zip(self.queue_head_slots_needed)
            .is_some_and(|(available, needed)| available >= needed);
        let start_stalled = self.queue_status.as_deref() == Some("start_stalled");

        if active == 0 && (queue_head_can_start || start_stalled) {
            Some(queued)
        } else {
            Some(0)
        }
    }

    fn queue_evidence(&self) -> Vec<String> {
        let mut evidence = BTreeSet::new();
        if let Some(count) = self.active_remote_build_count {
            evidence.insert(format!("active_remote_build_count:{count}"));
        }
        if let Some(count) = self.queued_remote_build_count {
            evidence.insert(format!("queued_remote_build_count:{count}"));
        }
        if let Some(slots) = self.slots_available {
            evidence.insert(format!("slots_available:{slots}"));
        }
        if let Some(slots) = self.queue_head_slots_needed {
            evidence.insert(format!("queue_head_slots_needed:{slots}"));
        }
        if let Some(seconds) = self.active_build_max_age_seconds {
            evidence.insert(format!("active_build_max_age_seconds:{seconds}"));
        }
        if let Some(status) = &self.queue_status {
            evidence.insert(format!("queue_status:{status}"));
        }
        if let Some(blocked) = self.head_of_line_blocked() {
            evidence.insert(format!("head_of_line_blocked:{blocked}"));
        }
        if let Some(count) = self.suspected_orphaned_queued_verifier_count()
            && count > 0
        {
            evidence.insert(format!("suspected_orphaned_queued_verifier_count:{count}"));
            evidence.insert("orphaned_queue_cleanup:coordination_first".to_owned());
            evidence.insert("orphaned_queue_cancelability:unknown".to_owned());
        }
        evidence.into_iter().collect()
    }

    fn queue_recommendation(&self) -> Option<&'static str> {
        if self
            .suspected_orphaned_queued_verifier_count()
            .is_some_and(|count| count > 0)
        {
            return Some("avoid_duplicate_verifier_until_orphaned_queue_is_explained");
        }
        if self.head_of_line_blocked() == Some(true) {
            return Some("prefer_static_work_until_queue_head_fits");
        }
        if self.slots_available == Some(0)
            && (self.active_remote_build_count.unwrap_or(0) > 0
                || self.queued_remote_build_count.unwrap_or(0) > 0)
        {
            return Some("wait_for_remote_capacity");
        }
        if self.remote_only_required && self.remote_only_safe == Some(false) {
            return Some("inspect_rch_status_before_launching_more_remote_work");
        }
        if self.remote_only_required && self.remote_only_safe == Some(true) {
            return Some("remote_verification_can_launch_when_ready");
        }
        None
    }

    fn admission_certificate(&self) -> SwarmNextActionAdmissionCertificate {
        let mut assumptions = vec![
            "remote_cargo_is_required_for_build_or_test_work",
            "certificate_is_advisory_and_never_mutates_rch_state",
        ];
        let mut proof_obligations = vec![
            "do_not_launch_duplicate_verifier_without_capacity_evidence",
            "record_admission_decision_in_closeout",
        ];

        let (action, rule_id, confidence) = if self
            .suspected_orphaned_queued_verifier_count()
            .is_some_and(|count| count > 0)
        {
            assumptions.push("queued_verifier_may_be_orphaned_when_start_is_stalled");
            proof_obligations.push("coordinate_before_queue_cleanup_or_retry");
            (
                "coordinate",
                "rch_admission.orphaned_queued_verifier",
                "medium",
            )
        } else if self.head_of_line_blocked() == Some(true) {
            assumptions.push("queue_head_needs_more_slots_than_currently_available");
            proof_obligations.push("prefer_static_work_until_queue_head_fits");
            ("static_work", "rch_admission.head_of_line_convoy", "high")
        } else if self.slots_available == Some(0)
            && (self.active_remote_build_count.unwrap_or(0) > 0
                || self.queued_remote_build_count.unwrap_or(0) > 0)
        {
            assumptions.push("zero_free_slots_means_new_remote_work_extends_queue_delay");
            proof_obligations.push("wait_or_batch_remote_verification");
            ("wait", "rch_admission.no_remote_capacity", "high")
        } else if self.remote_only_required && self.remote_only_safe == Some(false) {
            assumptions.push("remote_only_policy_is_required_but_current_rch_posture_is_not_safe");
            proof_obligations.push("inspect_rch_status_before_launching_more_remote_work");
            ("coordinate", "rch_admission.remote_only_unsafe", "medium")
        } else if self.remote_only_required && self.remote_only_safe == Some(true) {
            assumptions.push("remote_only_policy_is_satisfied_by_current_rch_posture");
            proof_obligations.push("use_rch_wrapper_for_any_cargo_command");
            ("queue", "rch_admission.remote_capacity_available", "medium")
        } else {
            assumptions.push("missing_rch_queue_fields_are_treated_conservatively");
            proof_obligations.push("collect_rch_status_before_remote_verification");
            ("coordinate", "rch_admission.insufficient_evidence", "low")
        };

        let estimate = self.service_time_estimate_with_history(&[]);

        SwarmNextActionAdmissionCertificate {
            schema: "ee.swarm_next_action.rch_admission_certificate.v1",
            action,
            rule_id,
            confidence,
            service_class: if self.remote_only_required {
                "cargo_verifier"
            } else {
                "unknown"
            },
            service_time_class: estimate.service_time_class,
            service_time_interval_ms: estimate.service_time_interval_ms,
            queue_risk_class: estimate.queue_risk_class,
            predictor_coverage: estimate.predictor_coverage,
            predictor_mode: estimate.predictor_mode,
            conservative_reason: estimate.conservative_reason,
            evidence: self.queue_evidence(),
            assumptions,
            proof_obligations,
            safety_invariant: "monotonic_queue_pressure_never_increases_aggression",
        }
    }

    #[must_use]
    pub fn service_time_estimate_with_history(
        &self,
        records: &[SwarmNextActionServiceTimeEvidenceRecord],
    ) -> SwarmNextActionServiceTimeEstimate {
        if records.is_empty() {
            return self.conservative_service_time_estimate("missing", "missing_history");
        }
        if records.len() < 5 {
            return self.conservative_service_time_estimate("sparse", "sparse_history");
        }
        if records
            .iter()
            .all(|record| record.observed_age_seconds > 7 * 24 * 60 * 60)
        {
            return self.conservative_service_time_estimate("stale", "stale_history");
        }
        if records.iter().any(|record| {
            record
                .failure_class
                .as_deref()
                .is_some_and(|class| matches!(class, "coverage_miss" | "prediction_miss"))
        }) {
            return self
                .conservative_service_time_estimate("miscalibrated", "miscalibrated_predictor");
        }
        if records
            .iter()
            .any(|record| record.duplicate_bead_attribution)
            && self.queued_remote_build_count.unwrap_or(0) > 0
        {
            return self.conservative_service_time_estimate("sparse", "duplicate_queued_verifier");
        }

        let mut durations = records
            .iter()
            .map(|record| record.duration_ms.saturating_add(record.queue_wait_ms))
            .collect::<Vec<_>>();
        durations.sort_unstable();
        let p50 = percentile(&durations, 50);
        let p90 = percentile(&durations, 90);
        if p90 > p50.saturating_mul(4).max(1) {
            return self.conservative_service_time_estimate("heavy_tailed", "heavy_tailed_history");
        }

        let lower = percentile(&durations, 20);
        let upper = p90.max(lower).saturating_add(30_000);
        SwarmNextActionServiceTimeEstimate {
            service_time_class: service_time_class_for_upper_bound(upper),
            service_time_interval_ms: SwarmNextActionServiceTimeIntervalMs { lower, upper },
            queue_risk_class: self.queue_risk_class(),
            predictor_coverage: "healthy",
            predictor_mode: "calibrated",
            conservative_reason: None,
        }
    }

    fn conservative_service_time_estimate(
        &self,
        predictor_coverage: &'static str,
        conservative_reason: &'static str,
    ) -> SwarmNextActionServiceTimeEstimate {
        let queue_risk_class = self.queue_risk_class();
        let (lower, upper) = match queue_risk_class {
            "low" => (60_000, 600_000),
            "medium" => (180_000, 900_000),
            "high" => (300_000, 1_800_000),
            "blocked" => (900_000, 3_600_000),
            _ => (300_000, 1_800_000),
        };
        SwarmNextActionServiceTimeEstimate {
            service_time_class: service_time_class_for_upper_bound(upper),
            service_time_interval_ms: SwarmNextActionServiceTimeIntervalMs { lower, upper },
            queue_risk_class,
            predictor_coverage,
            predictor_mode: "fallback",
            conservative_reason: Some(conservative_reason),
        }
    }

    fn queue_risk_class(&self) -> &'static str {
        if self
            .suspected_orphaned_queued_verifier_count()
            .is_some_and(|count| count > 0)
        {
            return "blocked";
        }
        if self.head_of_line_blocked() == Some(true)
            || (self.slots_available == Some(0)
                && (self.active_remote_build_count.unwrap_or(0) > 0
                    || self.queued_remote_build_count.unwrap_or(0) > 0))
            || (self.remote_only_required && self.remote_only_safe == Some(false))
        {
            return "high";
        }
        if self.queued_remote_build_count.unwrap_or(0) > 0 {
            return "medium";
        }
        if self.remote_only_required
            && self.remote_only_safe == Some(true)
            && self.slots_available.unwrap_or(0) > 0
        {
            return "low";
        }
        "unknown"
    }
}

fn percentile(sorted_values: &[u64], percentile: usize) -> u64 {
    debug_assert!(!sorted_values.is_empty());
    let last = sorted_values.len() - 1;
    let index = (last * percentile + 99) / 100;
    sorted_values[index.min(last)]
}

fn service_time_class_for_upper_bound(upper_ms: u64) -> &'static str {
    match upper_ms {
        0..=120_000 => "short",
        120_001..=600_000 => "medium",
        600_001..=1_800_000 => "long",
        _ => "long_tail",
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionEnvironmentSummary {
    pub cargo_target_externalized: bool,
    pub tmpdir_externalized: bool,
    pub external_agent_space_present: bool,
    pub disk_pressure_hint_count: usize,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionDegradation {
    pub code: String,
    pub source: String,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacket {
    pub schema: &'static str,
    pub packet_id: String,
    pub workspace: String,
    pub redaction_status: &'static str,
    pub observed_state_class: &'static str,
    pub recommended_action: SwarmWorkPacketRecommendedAction,
    pub candidates: Vec<SwarmWorkPacketCandidate>,
    pub coordination: SwarmWorkPacketCoordination,
    pub tracker_integrity: BeadsIntegrityReport,
    pub rch_proof_posture: SwarmWorkPacketRchProofPosture,
    pub verification: SwarmWorkPacketVerification,
    pub source_provenance: Vec<SwarmWorkPacketSourceProvenance>,
    pub mutation_policy: SwarmWorkPacketMutationPolicy,
    pub degraded: Vec<SwarmWorkPacketDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketRecommendedAction {
    pub action: &'static str,
    pub candidate_id: Option<String>,
    pub confidence: &'static str,
    pub safe_to_claim: Option<bool>,
    pub reasons: Vec<String>,
    pub proof_obligations: Vec<String>,
    pub suggested_commands: Vec<String>,
    pub suggested_command_actions: Vec<SwarmWorkPacketCommandAction>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketCommandAction {
    pub command_id: &'static str,
    pub display_command: String,
    pub argv: Vec<String>,
    pub shell_required: bool,
    pub copy_safety: &'static str,
    pub mutates_state: bool,
    pub required_substrate: &'static str,
    pub when: &'static str,
    pub rationale: &'static str,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketCandidate {
    pub id: String,
    pub title: String,
    pub source: &'static str,
    pub status: String,
    pub priority: Option<i64>,
    pub assignee: Option<String>,
    pub decision: &'static str,
    pub collision_risk: &'static str,
    pub unsafe_reasons: Vec<String>,
    pub stale_reasons: Vec<String>,
    pub source_refs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketClaimGate {
    pub schema: &'static str,
    pub gate_id: String,
    pub packet_id: String,
    pub workspace: String,
    pub redaction_status: &'static str,
    pub requested_candidate_id: Option<String>,
    pub verdict: &'static str,
    pub safe_to_claim: bool,
    pub selected_candidate: Option<SwarmWorkPacketClaimGateCandidate>,
    pub recommended_action: &'static str,
    pub recommended_safe_to_claim: Option<bool>,
    pub source_authority: SwarmWorkPacketClaimGateSourceAuthority,
    pub unsafe_reasons: Vec<String>,
    pub stale_reasons: Vec<String>,
    pub source_refs: Vec<String>,
    pub degraded_codes: Vec<String>,
    pub next_command_actions: Vec<SwarmWorkPacketCommandAction>,
    pub claim_command_action: Option<SwarmWorkPacketCommandAction>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketClaimGateCandidate {
    pub id: String,
    pub title: String,
    pub source: &'static str,
    pub status: String,
    pub priority: Option<i64>,
    pub assignee: Option<String>,
    pub decision: &'static str,
    pub collision_risk: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketClaimGateSourceAuthority {
    pub tracker_authoritative: bool,
    pub tracker_health: &'static str,
    pub agent_mail_status: &'static str,
    pub reservation_authoritative: Option<bool>,
    pub inbox_authoritative: Option<bool>,
    pub rch_remote_only_required: bool,
    pub rch_safe_to_launch_cargo_verification: Option<bool>,
    pub environment_verdict: &'static str,
    pub source_test_verdict: &'static str,
    pub remote_verification_admitted: Option<bool>,
    pub local_cargo_fallback_observed: Option<bool>,
    pub source_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketCoordination {
    pub active_claim_count: usize,
    pub dirty_path_count: usize,
    pub file_collision_count: usize,
    pub agent_mail: SwarmWorkPacketAgentMail,
    pub active_claims: Vec<SwarmWorkPacketActiveClaim>,
    pub file_collisions: Vec<SwarmWorkPacketFileCollision>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketAgentMail {
    pub status: &'static str,
    pub health_level: Option<&'static str>,
    pub unread_count: Option<u64>,
    pub ack_required_count: Option<u64>,
    pub degraded_codes: Vec<String>,
    pub recovery_mode: Option<&'static str>,
    pub archive_index_parity: Option<&'static str>,
    pub reservation_authoritative: Option<bool>,
    pub inbox_authoritative: Option<bool>,
    pub fallback_actions: Vec<SwarmWorkPacketAgentMailFallbackAction>,
    pub semantic_readiness: Option<SwarmWorkPacketAgentMailSemanticReadiness>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketAgentMailFallbackAction {
    pub kind: &'static str,
    pub summary: &'static str,
    pub command: Option<String>,
    pub command_action: Option<SwarmWorkPacketCommandAction>,
    pub manual_step: Option<&'static str>,
    pub repair_safety: SwarmWorkPacketRepairSafety,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketRepairSafety {
    pub risk_class: &'static str,
    pub preflight_command: Option<String>,
    pub requires_human_approval: bool,
    pub mutates_external_state: bool,
    pub mutates_tracker_state: bool,
    pub privacy_class: &'static str,
    pub next_action: &'static str,
    pub rule_id: &'static str,
    pub source: &'static str,
    pub reason_code: &'static str,
    pub evidence: Vec<&'static str>,
    pub preconditions: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketAgentMailSemanticReadiness {
    pub status: &'static str,
    pub reason: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketActiveClaim {
    pub bead_id: String,
    pub assignee: Option<String>,
    pub status: String,
    pub updated_at: Option<String>,
    pub source_refs: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketFileCollision {
    pub path_pattern: String,
    pub risk: &'static str,
    pub owners: Vec<String>,
    pub related_bead_ids: Vec<String>,
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketRchProofPosture {
    pub source_enabled: bool,
    pub remote_only_required: bool,
    pub posture: &'static str,
    pub healthy_worker_count: Option<u64>,
    pub safe_to_launch_cargo_verification: Option<bool>,
    pub local_fallback_prevented: bool,
    pub blocker_codes: Vec<String>,
    pub known_blockers: Vec<SwarmWorkPacketKnownBlocker>,
    pub retry_after: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketKnownBlocker {
    pub code: String,
    pub fingerprint: String,
    pub command_hash: Option<String>,
    pub message: Option<String>,
    pub remediation_bead: Option<String>,
    pub retry_after: Option<String>,
    pub remote_required: bool,
    pub local_fallback_refused: bool,
    pub degraded_codes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketVerification {
    pub required_commands: Vec<SwarmWorkPacketVerificationCommand>,
    pub static_checks: Vec<SwarmWorkPacketVerificationCommand>,
    pub closeout_evidence_required: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketVerificationCommand {
    pub command_id: &'static str,
    pub command_template: String,
    pub command_action: SwarmWorkPacketCommandAction,
    pub required_substrate: &'static str,
    pub when: &'static str,
    pub last_outcome: &'static str,
    pub last_command_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketSourceProvenance {
    pub source: String,
    pub collector: &'static str,
    pub status: &'static str,
    pub freshness: Option<String>,
    pub digest: Option<String>,
    pub redaction: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketMutationPolicy {
    pub side_effect_free: bool,
    pub claims_beads: bool,
    pub reserves_files: bool,
    pub sends_agent_mail: bool,
    pub runs_cargo: bool,
    pub stages_git: bool,
    pub deletes_files: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmWorkPacketDegradation {
    pub code: String,
    pub source: String,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

#[must_use]
pub fn collect_swarm_next_action_snapshot(
    options: &SwarmBriefCollectOptions,
    runner: &impl SwarmBriefCommandRunner,
) -> SwarmNextActionSnapshot {
    collect_swarm_next_action_snapshot_with_verifier_evidence(options, runner, &[])
}

#[must_use]
pub fn collect_swarm_next_action_snapshot_with_verifier_evidence(
    options: &SwarmBriefCollectOptions,
    runner: &impl SwarmBriefCommandRunner,
    verifier_evidence: &[SwarmNextActionRecentFirstError],
) -> SwarmNextActionSnapshot {
    let brief = collect_swarm_brief(options, runner);
    SwarmNextActionSnapshot::from_swarm_brief_with_verifier_evidence(&brief, verifier_evidence)
}

#[must_use]
pub fn collect_swarm_work_packet_with_verifier_evidence(
    options: &SwarmBriefCollectOptions,
    runner: &impl SwarmBriefCommandRunner,
    verifier_evidence: &[SwarmNextActionRecentFirstError],
) -> SwarmWorkPacket {
    let brief = collect_swarm_brief(options, runner);
    let tracker_integrity = collect_work_packet_tracker_integrity(options, runner, &brief);
    let mut verifier_evidence = verifier_evidence.to_vec();
    verifier_evidence.extend(collect_work_packet_ledger_verifier_evidence(
        &options.workspace,
    ));
    SwarmWorkPacket::from_swarm_brief_with_verifier_evidence_and_tracker_integrity(
        &brief,
        &verifier_evidence,
        tracker_integrity,
    )
}

fn collect_work_packet_ledger_verifier_evidence(
    workspace: &Path,
) -> Vec<SwarmNextActionRecentFirstError> {
    let database_path = workspace.join(".ee").join("ee.db");
    if !database_path.exists() {
        return Vec::new();
    }
    let Ok(connection) = DbConnection::open_file(&database_path) else {
        return Vec::new();
    };
    let canonical_workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let workspace_id = crate::core::curate::stable_workspace_id(&canonical_workspace);
    let Ok(report) =
        list_rch_verify_blockers(&connection, &workspace_id, None, &Utc::now().to_rfc3339())
    else {
        return Vec::new();
    };
    report
        .blockers
        .iter()
        .map(verifier_evidence_from_ledger_blocker)
        .collect()
}

fn collect_work_packet_tracker_integrity(
    options: &SwarmBriefCollectOptions,
    runner: &impl SwarmBriefCommandRunner,
    brief: &SwarmBriefReport,
) -> BeadsIntegrityReport {
    if !options
        .enabled_sources
        .contains(&SwarmBriefSourceKind::Beads)
    {
        return fallback_work_packet_tracker_integrity(brief, true);
    }

    runner
        .run(
            "br",
            &["doctor", "--json", "--no-db"],
            &options.workspace,
            options.command_timeout_ms,
        )
        .ok()
        .and_then(|output| {
            compose_integrity_report_from_br_doctor_json(
                &output.stdout,
                ".beads/issues.jsonl",
                ".beads/beads.db",
                true,
            )
            .ok()
        })
        .unwrap_or_else(|| fallback_work_packet_tracker_integrity(brief, true))
}

fn default_work_packet_tracker_integrity(brief: &SwarmBriefReport) -> BeadsIntegrityReport {
    fallback_work_packet_tracker_integrity(brief, false)
}

fn fallback_work_packet_tracker_integrity(
    brief: &SwarmBriefReport,
    force_non_authoritative: bool,
) -> BeadsIntegrityReport {
    let record_count = brief_beads_record_count(brief);
    let merge_artifact_paths: &[String] = &[];
    compose_integrity_report(BeadsIntegrityInputs {
        jsonl_path: ".beads/issues.jsonl",
        db_path: ".beads/beads.db",
        jsonl_record_count: record_count,
        db_record_count: record_count,
        auto_import_enabled: true,
        external_changes_pending_import: force_non_authoritative
            || brief_has_pending_beads_import(brief),
        dirty_issue_count: 0,
        merge_artifact_paths,
        jsonl_parse_error: None,
    })
}

fn brief_beads_record_count(brief: &SwarmBriefReport) -> u64 {
    (brief.beads.ready.len()
        + brief.beads.blocked.len()
        + brief.beads.in_progress.len()
        + brief.beads.deferred.len()) as u64
}

fn brief_has_pending_beads_import(brief: &SwarmBriefReport) -> bool {
    brief.degraded.iter().any(|degradation| {
        degradation.source == SwarmBriefSourceKind::Beads
            && degradation.code == "beads_tracker_stale"
    })
}

impl SwarmNextActionSnapshot {
    #[must_use]
    pub fn from_swarm_brief(brief: &SwarmBriefReport) -> Self {
        Self::from_swarm_brief_with_verifier_evidence(brief, &[])
    }

    #[must_use]
    pub fn from_swarm_brief_with_verifier_evidence(
        brief: &SwarmBriefReport,
        verifier_evidence: &[SwarmNextActionRecentFirstError],
    ) -> Self {
        let compile_health = compile_health_summary(brief, verifier_evidence);
        let blocked_by_compile_health = compile_health.safe_to_launch_rch == Some(false);
        let mut candidates = candidates_from_brief(brief, blocked_by_compile_health);
        candidates.sort_by(|left, right| {
            left.id
                .cmp(&right.id)
                .then_with(|| {
                    candidate_source_rank(left.source).cmp(&candidate_source_rank(right.source))
                })
                .then_with(|| right.score_milli.cmp(&left.score_milli))
                .then_with(|| left.title.cmp(&right.title))
        });
        candidates.dedup_by(|left, right| left.id == right.id);

        let mut dirty_paths = brief
            .dirty_files
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        dirty_paths.sort();
        dirty_paths.dedup();

        let mut degraded = brief
            .degraded
            .iter()
            .map(SwarmNextActionDegradation::from_brief)
            .collect::<Vec<_>>();
        degraded.sort();
        degraded.dedup();

        Self {
            schema: SWARM_NEXT_ACTION_SCHEMA_V1,
            workspace: brief.workspace.clone(),
            redaction_status: SWARM_NEXT_ACTION_REDACTION_STATUS,
            inputs: SwarmNextActionInputSummary {
                source_count: brief.sources.len(),
                ready_bead_count: brief.beads.ready.len(),
                in_progress_bead_count: brief.beads.in_progress.len(),
                blocked_bead_count: brief.beads.blocked.len(),
                bv_top_pick_count: brief
                    .bv
                    .as_ref()
                    .map_or(0, |summary| summary.top_picks.len()),
            },
            candidates,
            stale_work_proposals: stale_work_proposals_from_brief(brief),
            coordination: coordination_summary(brief),
            checkout: SwarmNextActionCheckoutSummary {
                dirty_path_count: dirty_paths.len(),
                dirty_paths,
            },
            compile_health,
            verification: verification_summary(brief, verifier_evidence),
            environment: environment_summary(brief),
            degraded,
        }
    }

    #[must_use]
    pub fn recommendation_cards(&self) -> Vec<SwarmNextActionRecommendationCard> {
        recommendation_cards_from_snapshot(self)
    }
}

impl SwarmWorkPacket {
    #[must_use]
    pub fn from_swarm_brief_with_verifier_evidence(
        brief: &SwarmBriefReport,
        verifier_evidence: &[SwarmNextActionRecentFirstError],
    ) -> Self {
        Self::from_swarm_brief_with_verifier_evidence_and_tracker_integrity(
            brief,
            verifier_evidence,
            default_work_packet_tracker_integrity(brief),
        )
    }

    #[must_use]
    pub fn from_swarm_brief_with_verifier_evidence_and_tracker_integrity(
        brief: &SwarmBriefReport,
        verifier_evidence: &[SwarmNextActionRecentFirstError],
        tracker_integrity: BeadsIntegrityReport,
    ) -> Self {
        let snapshot = SwarmNextActionSnapshot::from_swarm_brief_with_verifier_evidence(
            brief,
            verifier_evidence,
        );
        Self::from_brief_and_next_action_with_tracker_integrity(brief, &snapshot, tracker_integrity)
    }

    #[must_use]
    pub fn from_brief_and_next_action(
        brief: &SwarmBriefReport,
        snapshot: &SwarmNextActionSnapshot,
    ) -> Self {
        Self::from_brief_and_next_action_with_tracker_integrity(
            brief,
            snapshot,
            default_work_packet_tracker_integrity(brief),
        )
    }

    #[must_use]
    pub fn from_brief_and_next_action_with_tracker_integrity(
        brief: &SwarmBriefReport,
        snapshot: &SwarmNextActionSnapshot,
        tracker_integrity: BeadsIntegrityReport,
    ) -> Self {
        let mut degraded = snapshot
            .degraded
            .iter()
            .map(SwarmWorkPacketDegradation::from_next_action)
            .collect::<Vec<_>>();
        degraded.sort();
        degraded.dedup();

        let coordination = work_packet_coordination(brief, snapshot);
        let mut candidates = work_packet_candidates(brief, snapshot);
        candidates.sort();
        candidates.dedup();
        apply_coordination_collision_candidate_downgrade(&mut candidates, snapshot, &coordination);
        apply_agent_mail_authority_candidate_downgrade(&mut candidates, &coordination.agent_mail);
        apply_tracker_integrity_candidate_downgrade(&mut candidates, &tracker_integrity);

        let rch_proof_posture = work_packet_rch_proof_posture(snapshot, &degraded);
        let verification = work_packet_verification(snapshot, &rch_proof_posture);
        let source_provenance = work_packet_source_provenance(brief);
        let recommended_action = work_packet_recommended_action(
            snapshot,
            &candidates,
            &coordination.agent_mail,
            &rch_proof_posture,
            &tracker_integrity,
        );
        let observed_state_class =
            work_packet_observed_state_class(&coordination, &rch_proof_posture, &degraded);

        let mut packet = Self {
            schema: SWARM_WORK_PACKET_SCHEMA_V1,
            packet_id: String::new(),
            workspace: brief.workspace.clone(),
            redaction_status: SWARM_WORK_PACKET_REDACTION_STATUS,
            observed_state_class,
            recommended_action,
            candidates,
            coordination,
            tracker_integrity,
            rch_proof_posture,
            verification,
            source_provenance,
            mutation_policy: SwarmWorkPacketMutationPolicy::default_read_only(),
            degraded,
        };
        packet.packet_id = work_packet_id(&packet);
        packet
    }

    #[must_use]
    pub fn claim_gate(&self, requested_candidate_id: Option<&str>) -> SwarmWorkPacketClaimGate {
        let candidate = work_packet_claim_gate_candidate(self, requested_candidate_id);
        let recommended_safe_to_claim = candidate.map(|candidate| {
            work_packet_claim_gate_candidate_recommended_safe_to_claim(self, candidate)
        });
        let verdict = work_packet_claim_gate_verdict(self, requested_candidate_id, candidate);
        let safe_to_claim = verdict == "safe_to_claim" && recommended_safe_to_claim == Some(true);
        let actions = work_packet_suggested_command_actions(
            candidate
                .map(|candidate| candidate.id.as_str())
                .or(requested_candidate_id),
            candidate.map(|candidate| candidate.decision),
            &self.coordination.agent_mail,
            &self.rch_proof_posture,
            &self.tracker_integrity,
        );
        let mut next_command_actions = actions
            .iter()
            .filter(|action| !action.mutates_state)
            .cloned()
            .collect::<Vec<_>>();
        sort_work_packet_command_actions(&mut next_command_actions);
        let claim_command_action = safe_to_claim
            .then(|| {
                actions
                    .iter()
                    .find(|action| action.command_id == "bead_claim_candidate")
                    .cloned()
            })
            .flatten();
        let mut unsafe_reasons =
            work_packet_claim_gate_unsafe_reasons(self, requested_candidate_id, candidate, verdict);
        let mut stale_reasons = candidate
            .map(|candidate| candidate.stale_reasons.clone())
            .unwrap_or_default();
        let mut source_refs = candidate
            .map(|candidate| candidate.source_refs.clone())
            .unwrap_or_default();
        let mut degraded_codes = self
            .degraded
            .iter()
            .map(|degradation| degradation.code.clone())
            .collect::<Vec<_>>();
        unsafe_reasons.sort();
        unsafe_reasons.dedup();
        stale_reasons.sort();
        stale_reasons.dedup();
        source_refs.sort();
        source_refs.dedup();
        degraded_codes.sort();
        degraded_codes.dedup();
        let selected_candidate = candidate.map(SwarmWorkPacketClaimGateCandidate::from);
        let gate_id = work_packet_claim_gate_id(
            &self.packet_id,
            requested_candidate_id,
            verdict,
            safe_to_claim,
        );
        let source_authority_attestation = work_packet_claim_gate_attestation_summary(self);

        SwarmWorkPacketClaimGate {
            schema: SWARM_WORK_PACKET_CLAIM_GATE_SCHEMA_V1,
            gate_id,
            packet_id: self.packet_id.clone(),
            workspace: self.workspace.clone(),
            redaction_status: SWARM_WORK_PACKET_REDACTION_STATUS,
            requested_candidate_id: requested_candidate_id.map(str::to_owned),
            verdict,
            safe_to_claim,
            selected_candidate,
            recommended_action: self.recommended_action.action,
            recommended_safe_to_claim,
            source_authority: SwarmWorkPacketClaimGateSourceAuthority {
                tracker_authoritative: self.tracker_integrity.br_reads_authoritative,
                tracker_health: beads_integrity_health_label(self.tracker_integrity.health),
                agent_mail_status: self.coordination.agent_mail.status,
                reservation_authoritative: self.coordination.agent_mail.reservation_authoritative,
                inbox_authoritative: self.coordination.agent_mail.inbox_authoritative,
                rch_remote_only_required: self.rch_proof_posture.remote_only_required,
                rch_safe_to_launch_cargo_verification: self
                    .rch_proof_posture
                    .safe_to_launch_cargo_verification,
                environment_verdict: environment_attestation_verdict_label(
                    source_authority_attestation.environment_verdict,
                ),
                source_test_verdict: environment_attestation_source_test_verdict_label(
                    source_authority_attestation.source_test_verdict,
                ),
                remote_verification_admitted: source_authority_attestation
                    .remote_verification_admitted,
                local_cargo_fallback_observed: Some(
                    source_authority_attestation.local_cargo_fallback_observed,
                ),
                source_count: self.source_provenance.len(),
            },
            unsafe_reasons,
            stale_reasons,
            source_refs,
            degraded_codes,
            next_command_actions,
            claim_command_action,
        }
    }
}

impl From<&SwarmWorkPacketCandidate> for SwarmWorkPacketClaimGateCandidate {
    fn from(candidate: &SwarmWorkPacketCandidate) -> Self {
        Self {
            id: candidate.id.clone(),
            title: candidate.title.clone(),
            source: candidate.source,
            status: candidate.status.clone(),
            priority: candidate.priority,
            assignee: candidate.assignee.clone(),
            decision: candidate.decision,
            collision_risk: candidate.collision_risk,
        }
    }
}

impl SwarmWorkPacketMutationPolicy {
    const fn default_read_only() -> Self {
        Self {
            side_effect_free: true,
            claims_beads: false,
            reserves_files: false,
            sends_agent_mail: false,
            runs_cargo: false,
            stages_git: false,
            deletes_files: false,
        }
    }
}

impl SwarmWorkPacketDegradation {
    fn from_next_action(degradation: &SwarmNextActionDegradation) -> Self {
        Self {
            code: degradation.code.clone(),
            source: work_packet_source_label(&degradation.source),
            severity: degradation.severity,
            message: degradation.message.clone(),
            repair: degradation.repair.clone(),
        }
    }
}

fn work_packet_id(packet: &SwarmWorkPacket) -> String {
    let mut stable = packet.clone();
    stable.packet_id.clear();
    let bytes = serde_json::to_vec(&stable).unwrap_or_default();
    let digest = blake3::hash(&bytes).to_hex().to_string();
    format!("swarm_work_packet_{}", &digest[..24])
}

fn work_packet_claim_gate_id(
    packet_id: &str,
    requested_candidate_id: Option<&str>,
    verdict: &str,
    safe_to_claim: bool,
) -> String {
    let candidate = requested_candidate_id.unwrap_or("recommended");
    let material = format!("{packet_id}:{candidate}:{verdict}:{safe_to_claim}");
    let digest = blake3::hash(material.as_bytes()).to_hex().to_string();
    format!("swarm_work_packet_claim_gate_{}", &digest[..24])
}

fn work_packet_claim_gate_candidate<'a>(
    packet: &'a SwarmWorkPacket,
    requested_candidate_id: Option<&str>,
) -> Option<&'a SwarmWorkPacketCandidate> {
    if let Some(candidate_id) = requested_candidate_id {
        return packet
            .candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id);
    }
    packet
        .recommended_action
        .candidate_id
        .as_deref()
        .and_then(|candidate_id| {
            packet
                .candidates
                .iter()
                .find(|candidate| candidate.id == candidate_id)
        })
        .or_else(|| packet.candidates.first())
}

fn work_packet_claim_gate_candidate_recommended_safe_to_claim(
    packet: &SwarmWorkPacket,
    candidate: &SwarmWorkPacketCandidate,
) -> bool {
    candidate.decision == "safe_to_claim"
        && packet.recommended_action.safe_to_claim == Some(true)
        && packet.recommended_action.candidate_id.as_deref() == Some(candidate.id.as_str())
        && packet.tracker_integrity.br_reads_authoritative
        && !agent_mail_blocks_claim(&packet.coordination.agent_mail)
        && work_packet_rch_allows_claim(&packet.rch_proof_posture)
}

fn work_packet_claim_gate_verdict(
    packet: &SwarmWorkPacket,
    requested_candidate_id: Option<&str>,
    candidate: Option<&SwarmWorkPacketCandidate>,
) -> &'static str {
    let Some(candidate) = candidate else {
        return if requested_candidate_id.is_some() {
            "candidate_not_found"
        } else {
            "no_candidate"
        };
    };
    if candidate.decision != "safe_to_claim" {
        return candidate.decision;
    }
    if !packet.tracker_integrity.br_reads_authoritative {
        return "external_state_required";
    }
    if agent_mail_blocks_claim(&packet.coordination.agent_mail) {
        return "external_state_required";
    }
    if work_packet_rch_remote_verification_reason(&packet.rch_proof_posture).is_some() {
        return "blocked_by_verification";
    }
    if !work_packet_claim_gate_candidate_recommended_safe_to_claim(packet, candidate) {
        return "coordinate_first";
    }
    "safe_to_claim"
}

fn work_packet_claim_gate_unsafe_reasons(
    packet: &SwarmWorkPacket,
    requested_candidate_id: Option<&str>,
    candidate: Option<&SwarmWorkPacketCandidate>,
    verdict: &str,
) -> Vec<String> {
    let mut reasons = candidate
        .map(|candidate| candidate.unsafe_reasons.clone())
        .unwrap_or_default();
    match candidate {
        Some(candidate) if candidate.decision != "safe_to_claim" => {
            reasons.push(format!("candidate_decision:{}", candidate.decision));
        }
        None => {
            if let Some(candidate_id) = requested_candidate_id {
                reasons.push(format!("candidate_not_found:{candidate_id}"));
            } else {
                reasons.push("no_candidate_available".to_owned());
            }
        }
        _ => {}
    }
    if !packet.tracker_integrity.br_reads_authoritative {
        reasons.push(format!(
            "beads_tracker_not_authoritative:{}",
            beads_integrity_health_label(packet.tracker_integrity.health)
        ));
    }
    if agent_mail_blocks_claim(&packet.coordination.agent_mail) {
        reasons.push(AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE.to_owned());
        if packet.coordination.agent_mail.reservation_authoritative != Some(true) {
            reasons.push("reservation_evidence_not_authoritative".to_owned());
        }
        if packet.coordination.agent_mail.inbox_authoritative != Some(true) {
            reasons.push("inbox_evidence_not_authoritative".to_owned());
        }
    }
    if let Some(reason) = work_packet_rch_remote_verification_reason(&packet.rch_proof_posture) {
        reasons.push(reason.to_owned());
    }
    if packet.recommended_action.safe_to_claim != Some(true) {
        reasons.push(format!(
            "packet_recommendation_not_claim_safe:{}",
            packet.recommended_action.action
        ));
    }
    if let Some(candidate) = candidate {
        match packet.recommended_action.candidate_id.as_deref() {
            Some(recommended) if recommended != candidate.id => {
                reasons.push(format!(
                    "packet_recommendation_candidate_mismatch:{recommended}:{}",
                    candidate.id
                ));
            }
            None => reasons.push("packet_recommendation_candidate_missing".to_owned()),
            _ => {}
        }
    }
    if verdict != "safe_to_claim" && !reasons.iter().any(|reason| reason == verdict) {
        reasons.push(format!("gate_verdict:{verdict}"));
    }
    reasons
}

fn work_packet_claim_gate_attestation_summary(
    packet: &SwarmWorkPacket,
) -> EnvironmentAttestationSummary {
    let environment_verdict = work_packet_claim_gate_environment_verdict(packet);
    let remote_verification_admitted = work_packet_claim_gate_remote_verification_admitted(packet);
    let local_cargo_fallback_observed = packet
        .degraded
        .iter()
        .any(|degradation| degradation.code == "local_cargo_bypass_detected");
    EnvironmentAttestationSummary {
        safe_to_claim: matches!(
            environment_verdict,
            EnvironmentAttestationVerdict::SafeToClaim
                | EnvironmentAttestationVerdict::RemoteVerificationAdmitted
        ),
        remote_verification_admitted,
        source_test_verdict: work_packet_claim_gate_source_test_verdict(environment_verdict),
        environment_verdict,
        local_cargo_fallback_observed,
    }
}

fn work_packet_claim_gate_environment_verdict(
    packet: &SwarmWorkPacket,
) -> EnvironmentAttestationVerdict {
    if packet
        .degraded
        .iter()
        .any(|degradation| degradation.code == "local_cargo_bypass_detected")
    {
        return EnvironmentAttestationVerdict::LocalCargoBypassDetected;
    }
    if packet.rch_proof_posture.safe_to_launch_cargo_verification == Some(false)
        || packet.rch_proof_posture.blocker_codes.iter().any(|code| {
            code == "rch_worker_topology_blocked"
                || code == "rch_remote_required_fallback_prevented"
        })
    {
        return EnvironmentAttestationVerdict::ProofEnvironmentBlocked;
    }
    if !packet.tracker_integrity.br_reads_authoritative {
        return EnvironmentAttestationVerdict::TrackerStale;
    }
    if work_packet_has_active_reservation_conflict(packet) {
        return EnvironmentAttestationVerdict::UnsafeDueToConflict;
    }
    if agent_mail_blocks_claim(&packet.coordination.agent_mail)
        || work_packet_has_coordination_degradation(packet)
    {
        return EnvironmentAttestationVerdict::CoordinateBeforeClaim;
    }
    if work_packet_claim_gate_remote_verification_admitted(packet) == Some(true) {
        return EnvironmentAttestationVerdict::RemoteVerificationAdmitted;
    }
    if packet.rch_proof_posture.remote_only_required {
        return EnvironmentAttestationVerdict::SourceAuthorityAmbiguous;
    }
    EnvironmentAttestationVerdict::SafeToClaim
}

fn work_packet_claim_gate_source_test_verdict(
    environment_verdict: EnvironmentAttestationVerdict,
) -> EnvironmentAttestationSourceTestVerdict {
    if environment_verdict == EnvironmentAttestationVerdict::ProofEnvironmentBlocked {
        EnvironmentAttestationSourceTestVerdict::EnvironmentBlockedBeforeSource
    } else {
        EnvironmentAttestationSourceTestVerdict::NotEvaluated
    }
}

fn work_packet_claim_gate_remote_verification_admitted(packet: &SwarmWorkPacket) -> Option<bool> {
    packet.rch_proof_posture.safe_to_launch_cargo_verification
}

fn environment_attestation_verdict_label(verdict: EnvironmentAttestationVerdict) -> &'static str {
    match verdict {
        EnvironmentAttestationVerdict::SafeToClaim => "safe_to_claim",
        EnvironmentAttestationVerdict::CoordinateBeforeClaim => "coordinate_before_claim",
        EnvironmentAttestationVerdict::UnsafeDueToConflict => "unsafe_due_to_conflict",
        EnvironmentAttestationVerdict::RemoteVerificationAdmitted => "remote_verification_admitted",
        EnvironmentAttestationVerdict::ProofEnvironmentBlocked => "proof_environment_blocked",
        EnvironmentAttestationVerdict::SourceAuthorityAmbiguous => "source_authority_ambiguous",
        EnvironmentAttestationVerdict::StaleBinarySuspected => "stale_binary_suspected",
        EnvironmentAttestationVerdict::TrackerStale => "tracker_stale",
        EnvironmentAttestationVerdict::LocalCargoBypassDetected => "local_cargo_bypass_detected",
        EnvironmentAttestationVerdict::UnknownInsufficientEvidence => {
            "unknown_insufficient_evidence"
        }
    }
}

fn environment_attestation_source_test_verdict_label(
    verdict: EnvironmentAttestationSourceTestVerdict,
) -> &'static str {
    match verdict {
        EnvironmentAttestationSourceTestVerdict::NotEvaluated => "not_evaluated",
        EnvironmentAttestationSourceTestVerdict::SourceNotTested => "source_not_tested",
        EnvironmentAttestationSourceTestVerdict::SourcePassed => "source_passed",
        EnvironmentAttestationSourceTestVerdict::SourceFailed => "source_failed",
        EnvironmentAttestationSourceTestVerdict::EnvironmentBlockedBeforeSource => {
            "environment_blocked_before_source"
        }
        EnvironmentAttestationSourceTestVerdict::StaleSource => "stale_source",
        EnvironmentAttestationSourceTestVerdict::Unknown => "unknown",
    }
}

fn work_packet_has_active_reservation_conflict(packet: &SwarmWorkPacket) -> bool {
    packet
        .coordination
        .file_collisions
        .iter()
        .any(|collision| !collision.owners.is_empty())
}

fn work_packet_has_coordination_degradation(packet: &SwarmWorkPacket) -> bool {
    packet.degraded.iter().any(|degradation| {
        matches!(
            degradation.code.as_str(),
            "agent_mail_unavailable"
                | "agent_mail_semantic_readiness_failed"
                | "agent_mail_probe_mismatch"
                | "bv_command_timeout"
                | "bv_no_output"
                | "bv_unavailable"
                | "bv_recommendation_stale"
                | "memory_drift_source_unverifiable"
        )
    })
}

fn work_packet_candidates(
    brief: &SwarmBriefReport,
    snapshot: &SwarmNextActionSnapshot,
) -> Vec<SwarmWorkPacketCandidate> {
    let cards_by_candidate = snapshot
        .recommendation_cards()
        .into_iter()
        .filter_map(|card| card.candidate_id.clone().map(|id| (id, card)))
        .collect::<BTreeMap<_, _>>();
    let stale_by_bead = snapshot
        .stale_work_proposals
        .iter()
        .map(|proposal| (proposal.bead_id.as_str(), proposal))
        .collect::<BTreeMap<_, _>>();

    snapshot
        .candidates
        .iter()
        .map(|candidate| {
            let card = cards_by_candidate.get(&candidate.id);
            let stale = stale_by_bead.get(candidate.id.as_str());
            let decision = work_packet_candidate_decision(
                candidate,
                card.map(|card| card.decision),
                stale.map(|proposal| proposal.decision),
                brief,
                snapshot,
            );
            let mut unsafe_reasons =
                card.map_or_else(Vec::new, |card| card.do_not_take_because.clone());
            if decision == "unsafe_due_to_conflict" {
                unsafe_reasons.extend(work_packet_candidate_conflict_evidence(
                    candidate, brief, snapshot,
                ));
                unsafe_reasons.sort();
                unsafe_reasons.dedup();
            }
            if decision == "release_operator_required" {
                unsafe_reasons.extend(candidate_release_operator_reasons(candidate));
                unsafe_reasons.sort();
                unsafe_reasons.dedup();
            }
            let stale_reasons = stale.map_or_else(Vec::new, |proposal| proposal.evidence.clone());
            SwarmWorkPacketCandidate {
                id: candidate.id.clone(),
                title: candidate.title.clone(),
                source: work_packet_candidate_source(candidate.source),
                status: candidate.status.clone(),
                priority: candidate.priority,
                assignee: candidate.assignee.clone(),
                decision,
                collision_risk: work_packet_collision_risk(candidate, snapshot),
                unsafe_reasons,
                stale_reasons,
                source_refs: work_packet_candidate_source_refs(candidate),
            }
        })
        .collect()
}

fn apply_tracker_integrity_candidate_downgrade(
    candidates: &mut [SwarmWorkPacketCandidate],
    tracker_integrity: &BeadsIntegrityReport,
) {
    if tracker_integrity.br_reads_authoritative {
        return;
    }

    let unsafe_reason = format!(
        "beads_tracker_not_authoritative:{}",
        beads_integrity_health_label(tracker_integrity.health)
    );
    for candidate in candidates {
        if candidate.decision == "safe_to_claim" {
            candidate.decision = "external_state_required";
        }
        candidate.unsafe_reasons.push(unsafe_reason.clone());
        candidate.unsafe_reasons.sort();
        candidate.unsafe_reasons.dedup();
    }
}

fn apply_agent_mail_authority_candidate_downgrade(
    candidates: &mut [SwarmWorkPacketCandidate],
    agent_mail: &SwarmWorkPacketAgentMail,
) {
    if !agent_mail_blocks_claim(agent_mail) {
        return;
    }

    let unsafe_reason = AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE;
    for candidate in candidates {
        if candidate.decision == "safe_to_claim" {
            candidate.decision = "external_state_required";
        }
        candidate.unsafe_reasons.push(unsafe_reason.to_owned());
        candidate.unsafe_reasons.sort();
        candidate.unsafe_reasons.dedup();
    }
}

fn apply_coordination_collision_candidate_downgrade(
    candidates: &mut [SwarmWorkPacketCandidate],
    snapshot: &SwarmNextActionSnapshot,
    coordination: &SwarmWorkPacketCoordination,
) {
    if snapshot.checkout.dirty_paths.is_empty() && coordination.file_collisions.is_empty() {
        return;
    }

    let source_candidates = snapshot
        .candidates
        .iter()
        .map(|candidate| (candidate.id.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();

    for candidate in candidates {
        let Some(source_candidate) = source_candidates.get(candidate.id.as_str()) else {
            continue;
        };
        let (reasons, collision_risk) =
            candidate_coordination_collision_reasons(source_candidate, snapshot, coordination);
        if reasons.is_empty() {
            continue;
        }
        if candidate.decision == "safe_to_claim" {
            candidate.decision = "unsafe_due_to_conflict";
        }
        if collision_risk == "high" || candidate.collision_risk == "none" {
            candidate.collision_risk = collision_risk;
        }
        candidate.unsafe_reasons.extend(reasons);
        candidate.unsafe_reasons.sort();
        candidate.unsafe_reasons.dedup();
    }
}

fn candidate_coordination_collision_reasons(
    candidate: &SwarmNextActionCandidate,
    snapshot: &SwarmNextActionSnapshot,
    coordination: &SwarmWorkPacketCoordination,
) -> (Vec<String>, &'static str) {
    let likely_paths = candidate_likely_edit_paths(candidate);
    if likely_paths.is_empty() {
        return (Vec::new(), "none");
    }

    let mut reasons = BTreeSet::new();
    let mut high_risk = false;
    for collision in &coordination.file_collisions {
        if !likely_paths
            .iter()
            .any(|path| path_patterns_overlap(path, &collision.path_pattern))
        {
            continue;
        }
        if collision.risk == "high" {
            high_risk = true;
        }
        reasons.insert(format!(
            "file_collision:{}:{}",
            collision.risk, collision.path_pattern
        ));
        for owner in &collision.owners {
            reasons.insert(format!(
                "file_collision_owner:{owner}:{}",
                collision.path_pattern
            ));
        }
        for bead_id in &collision.related_bead_ids {
            reasons.insert(format!("file_collision_related_bead:{bead_id}"));
        }
    }

    for dirty_path in &snapshot.checkout.dirty_paths {
        if likely_paths
            .iter()
            .any(|path| path_patterns_overlap(path, dirty_path))
        {
            reasons.insert(format!("dirty_path_overlap:{dirty_path}"));
        }
    }

    let collision_risk = if high_risk {
        "high"
    } else if reasons.is_empty() {
        "none"
    } else {
        "medium"
    };
    (reasons.into_iter().collect(), collision_risk)
}

fn candidate_likely_edit_paths(candidate: &SwarmNextActionCandidate) -> Vec<String> {
    let decision = if candidate.status == "unknown" {
        "new_bead_recommended"
    } else {
        "refine_existing_bead"
    };
    let mut paths = suggested_reservations_for_candidate(candidate, decision)
        .into_iter()
        .map(|reservation| reservation.path_pattern)
        .filter(|path| path != ".beads/issues.jsonl")
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn path_patterns_overlap(left: &str, right: &str) -> bool {
    path_matches_pattern(left, right) || path_matches_pattern(right, left)
}

fn work_packet_candidate_source(source: &'static str) -> &'static str {
    match source {
        "bv_top_pick" => "bv_top_pick",
        "beads_ready" => "beads_ready",
        _ => "manual",
    }
}

fn work_packet_candidate_decision(
    candidate: &SwarmNextActionCandidate,
    card_decision: Option<&'static str>,
    stale_decision: Option<&'static str>,
    brief: &SwarmBriefReport,
    snapshot: &SwarmNextActionSnapshot,
) -> &'static str {
    if card_decision == Some("duplicate_rejected") {
        return "skip";
    }
    if !candidate.blocked_by.is_empty() {
        return "blocked_by_dependency";
    }
    match candidate.status.as_str() {
        "blocked" => return "blocked_by_dependency",
        "deferred" => return "external_state_required",
        _ => {}
    }
    if !candidate_release_operator_reasons(candidate).is_empty() {
        return "release_operator_required";
    }
    if candidate.blocked_by_compile_health {
        return "blocked_by_verification";
    }
    match stale_decision {
        Some("reopenSuggested") => return "stale_but_reclaimable",
        Some("contactSuggested") => return "stale_review",
        Some("leaveAloneActive") => return "already_owned",
        _ => {}
    }
    if candidate.assignee.is_some() || card_decision == Some("blocked_by_owner") {
        return "already_owned";
    }
    if candidate_is_rollup(candidate) {
        return "blocked_rollup";
    }
    if work_packet_candidate_conflict_present(candidate, brief, snapshot) {
        return "unsafe_due_to_conflict";
    }
    match card_decision {
        Some("new_bead_recommended" | "refine_existing_bead") => "safe_to_claim",
        Some("reuse_recent_evidence") => "blocked_by_verification",
        Some("no_action_recommended") => "blocked",
        _ => "safe_to_claim",
    }
}

fn candidate_release_operator_reasons(candidate: &SwarmNextActionCandidate) -> Vec<String> {
    let title = candidate.title.to_ascii_lowercase();
    let mut reasons = BTreeSet::new();
    if title.contains("publish-dep:") || title.contains("upstream-publish:") {
        reasons.insert("release_operator_required:dependency_publish");
    }
    if title.contains("crates.io") || title.contains("cargo publish") {
        reasons.insert("release_operator_required:crates_io_publish");
    }
    if title.contains("homebrew") || title.contains("publish_flip") {
        reasons.insert("release_operator_required:distribution_publish");
    }
    if title.contains("tag recovery")
        || title.contains("signed release")
        || title.contains("release signing")
    {
        reasons.insert("release_operator_required:release_authority");
    }
    if title.contains("operator approval")
        || title.contains("credential-required")
        || title.contains("credentials")
    {
        reasons.insert("release_operator_required:operator_approval");
    }
    reasons.into_iter().map(str::to_owned).collect()
}

fn work_packet_candidate_conflict_present(
    candidate: &SwarmNextActionCandidate,
    brief: &SwarmBriefReport,
    snapshot: &SwarmNextActionSnapshot,
) -> bool {
    !work_packet_candidate_conflict_evidence(candidate, brief, snapshot).is_empty()
}

fn work_packet_candidate_conflict_evidence(
    candidate: &SwarmNextActionCandidate,
    brief: &SwarmBriefReport,
    snapshot: &SwarmNextActionSnapshot,
) -> Vec<String> {
    let likely_paths = candidate_likely_edit_paths(candidate);
    if likely_paths.is_empty() {
        return work_packet_global_conflict_evidence(brief, snapshot);
    }

    let mut evidence = BTreeSet::new();
    let dirty_overlaps = snapshot
        .checkout
        .dirty_paths
        .iter()
        .filter(|dirty_path| {
            likely_paths
                .iter()
                .any(|path| path_patterns_overlap(path, dirty_path))
        })
        .collect::<Vec<_>>();
    if !dirty_overlaps.is_empty() {
        evidence.insert(format!(
            "dirty_checkout_path_count:{}",
            snapshot.checkout.dirty_path_count
        ));
        for dirty_path in dirty_overlaps {
            evidence.insert(format!("dirty_path_overlap:{dirty_path}"));
        }
    }
    for risk in &brief.file_surface_risks {
        if !likely_paths
            .iter()
            .any(|path| path_patterns_overlap(path, &risk.path_pattern))
        {
            continue;
        }
        if risk.severity == "high" {
            evidence.insert(format!("high_risk_dirty_surface:{}", risk.path_pattern));
        }
        if !risk.reservation_holders.is_empty() {
            evidence.insert(format!("reservation_collision:{}", risk.path_pattern));
        }
        if !risk.related_bead_ids.is_empty() {
            evidence.insert(format!("related_bead_collision:{}", risk.path_pattern));
        }
    }
    evidence.into_iter().collect()
}

fn work_packet_global_conflict_evidence(
    brief: &SwarmBriefReport,
    snapshot: &SwarmNextActionSnapshot,
) -> Vec<String> {
    let mut evidence = BTreeSet::new();
    if snapshot.checkout.dirty_path_count > 0 {
        evidence.insert(format!(
            "dirty_checkout_path_count:{}",
            snapshot.checkout.dirty_path_count
        ));
    }
    for risk in &brief.file_surface_risks {
        if risk.severity == "high" {
            evidence.insert(format!("high_risk_dirty_surface:{}", risk.path_pattern));
        }
        if !risk.reservation_holders.is_empty() {
            evidence.insert(format!("reservation_collision:{}", risk.path_pattern));
        }
        if !risk.related_bead_ids.is_empty() {
            evidence.insert(format!("related_bead_collision:{}", risk.path_pattern));
        }
    }
    evidence.into_iter().collect()
}

fn work_packet_collision_risk(
    candidate: &SwarmNextActionCandidate,
    snapshot: &SwarmNextActionSnapshot,
) -> &'static str {
    if candidate.assignee.is_some()
        || snapshot
            .compile_health
            .blockers
            .iter()
            .any(|blocker| blocker.owner_agent.is_some())
    {
        "high"
    } else if candidate.blocked_by_compile_health || snapshot.checkout.dirty_path_count > 0 {
        "medium"
    } else {
        "none"
    }
}

fn work_packet_candidate_source_refs(candidate: &SwarmNextActionCandidate) -> Vec<String> {
    let mut refs = BTreeSet::new();
    if candidate.status != "unknown" {
        refs.insert(format!("br://{}", candidate.id));
    }
    if candidate.source == "bv_top_pick" {
        refs.insert(format!("bv://top-pick/{}", candidate.id));
    }
    refs.into_iter().collect()
}

fn work_packet_coordination(
    brief: &SwarmBriefReport,
    snapshot: &SwarmNextActionSnapshot,
) -> SwarmWorkPacketCoordination {
    let mut active_claims = brief
        .beads
        .in_progress
        .iter()
        .map(|bead| SwarmWorkPacketActiveClaim {
            bead_id: bead.id.clone(),
            assignee: bead.assignee.clone(),
            status: bead.status.clone(),
            updated_at: None,
            source_refs: vec![format!("br://{}", bead.id)],
        })
        .collect::<Vec<_>>();
    active_claims.sort();
    active_claims.dedup();

    let mut file_collisions = brief
        .file_surface_risks
        .iter()
        .filter(|risk| {
            !risk.reservation_holders.is_empty()
                || !risk.related_bead_ids.is_empty()
                || risk.severity == "high"
        })
        .map(|risk| SwarmWorkPacketFileCollision {
            path_pattern: risk.path_pattern.clone(),
            risk: work_packet_file_collision_risk(&risk.severity),
            owners: risk.reservation_holders.clone(),
            related_bead_ids: risk.related_bead_ids.clone(),
            evidence: risk.evidence.clone(),
        })
        .collect::<Vec<_>>();
    file_collisions.sort();
    file_collisions.dedup();

    SwarmWorkPacketCoordination {
        active_claim_count: active_claims.len(),
        dirty_path_count: snapshot.checkout.dirty_path_count,
        file_collision_count: file_collisions.len(),
        agent_mail: work_packet_agent_mail(brief, snapshot),
        active_claims,
        file_collisions,
    }
}

fn work_packet_file_collision_risk(severity: &str) -> &'static str {
    match severity {
        "high" | "critical" => "high",
        "medium" | "warning" => "medium",
        _ => "low",
    }
}

fn work_packet_agent_mail(
    brief: &SwarmBriefReport,
    snapshot: &SwarmNextActionSnapshot,
) -> SwarmWorkPacketAgentMail {
    let source = brief
        .sources
        .iter()
        .find(|source| source.source == SwarmBriefSourceKind::AgentMail);
    let mut degraded_codes = snapshot
        .degraded
        .iter()
        .filter(|degradation| degradation.source == "agent_mail")
        .map(|degradation| degradation.code.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let status = source.map_or("skipped", |source| match source.status.as_str() {
        "ready" => "fresh",
        "degraded" => "degraded_read_only",
        "unavailable" => "unavailable",
        _ => "skipped",
    });
    let semantic_failure_reason = agent_mail_semantic_failure_reason(snapshot);
    let status = if semantic_failure_reason.is_some() {
        "semantic_readiness_failed"
    } else {
        status
    };
    if status == "semantic_readiness_failed"
        && !degraded_codes
            .iter()
            .any(|code| code == AGENT_MAIL_UNAVAILABLE_CODE)
    {
        degraded_codes.push(AGENT_MAIL_UNAVAILABLE_CODE.to_owned());
        degraded_codes.sort();
        degraded_codes.dedup();
    }
    let health_level = match status {
        "fresh" => Some("green"),
        "semantic_readiness_failed" => agent_mail_health_level_from_semantic_failure(snapshot),
        "degraded_read_only" | "unavailable" => Some("red"),
        _ => None,
    };
    let counts_available = status == "fresh";
    let reservation_authoritative = agent_mail_authoritative_flag(status);
    let inbox_authoritative = agent_mail_authoritative_flag(status);
    let semantic_readiness =
        semantic_failure_reason.map(|reason| SwarmWorkPacketAgentMailSemanticReadiness {
            status: "fail",
            reason: Some(reason),
        });
    SwarmWorkPacketAgentMail {
        status,
        health_level,
        unread_count: counts_available.then_some(snapshot.coordination.unread_inbox_count),
        ack_required_count: counts_available.then_some(snapshot.coordination.ack_required_count),
        degraded_codes,
        recovery_mode: agent_mail_recovery_mode(status),
        archive_index_parity: agent_mail_archive_index_parity(status, snapshot),
        reservation_authoritative,
        inbox_authoritative,
        fallback_actions: agent_mail_fallback_actions(status),
        semantic_readiness,
    }
}

fn agent_mail_semantic_failure_reason(snapshot: &SwarmNextActionSnapshot) -> Option<&'static str> {
    snapshot.degraded.iter().find_map(|degradation| {
        if degradation.source != "agent_mail"
            || degradation.code != AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE
        {
            return None;
        }
        Some(agent_mail_semantic_reason_from_message(
            &degradation.message,
        ))
    })
}

fn agent_mail_semantic_reason_from_message(message: &str) -> &'static str {
    if message.contains("malformed_sqlite") {
        "malformed_sqlite"
    } else if message.contains("archive_corruption") {
        "archive_corruption"
    } else if message.contains("index_rebuild_required") {
        "index_rebuild_required"
    } else if message.contains("permission_denied") {
        "permission_denied"
    } else {
        "unknown"
    }
}

fn agent_mail_health_level_from_semantic_failure(
    snapshot: &SwarmNextActionSnapshot,
) -> Option<&'static str> {
    snapshot
        .degraded
        .iter()
        .filter(|degradation| {
            degradation.source == "agent_mail"
                && degradation.code == AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE
        })
        .find_map(|degradation| {
            let message = degradation.message.as_str();
            if message.contains("healthLevel=green") {
                Some("green")
            } else if message.contains("healthLevel=yellow") {
                Some("yellow")
            } else if message.contains("healthLevel=red") {
                Some("red")
            } else {
                None
            }
        })
        .or(Some("green"))
}

fn agent_mail_authoritative_flag(status: &str) -> Option<bool> {
    match status {
        "fresh" | "healthy" => Some(true),
        "degraded_read_only"
        | "archive_ahead_of_sqlite"
        | "inbox_unavailable"
        | "reservation_unavailable"
        | "outbox_only"
        | "semantic_readiness_failed"
        | "unreachable"
        | "unavailable" => Some(false),
        _ => None,
    }
}

fn agent_mail_recovery_mode(status: &str) -> Option<&'static str> {
    match status {
        "fresh" | "healthy" => Some("none"),
        "semantic_readiness_failed" => Some("manual_coordination"),
        "degraded_read_only" | "archive_ahead_of_sqlite" => Some("proceed_via_beads"),
        "unavailable" | "unreachable" => Some("wait_for_repair"),
        "inbox_unavailable" | "reservation_unavailable" | "outbox_only" => {
            Some("manual_coordination")
        }
        _ => None,
    }
}

fn agent_mail_archive_index_parity(
    status: &str,
    snapshot: &SwarmNextActionSnapshot,
) -> Option<&'static str> {
    if status == "fresh" || status == "healthy" {
        return Some("aligned");
    }
    if snapshot
        .degraded
        .iter()
        .any(|degradation| degradation.code == "archive_index_parity_drift")
    {
        return Some("archive_ahead");
    }
    match status {
        "semantic_readiness_failed" | "degraded_read_only" | "unavailable" => Some("unknown"),
        _ => None,
    }
}

fn agent_mail_fallback_actions(status: &str) -> Vec<SwarmWorkPacketAgentMailFallbackAction> {
    if status == "fresh" || status == "healthy" || status == "skipped" {
        return Vec::new();
    }

    let mut actions = vec![
        agent_mail_fallback_action(
            "manual_coordination",
            "Coordinate file ownership outside Agent Mail while reservation and inbox reads are unavailable.",
            None,
            None,
            Some(
                "Confirm lane ownership in the active coordination channel before touching shared paths.",
            ),
        ),
        agent_mail_fallback_action(
            "retry_later",
            "Retry Agent Mail health after the storage layer or index is repaired.",
            None,
            None,
            Some("Re-run the work-packet collector after Agent Mail reads recover."),
        ),
        agent_mail_fallback_action(
            "switch_to_static_work",
            "Prefer static or docs-first work while coordination authority is unavailable.",
            None,
            None,
            Some("Avoid claiming peer-touched lanes until Agent Mail reads recover."),
        ),
    ];
    if status == "semantic_readiness_failed" {
        actions.push(agent_mail_fallback_action(
            "beads_comment",
            "Record the Agent Mail semantic-readiness failure in Beads and coordinate there until storage repair completes.",
            None,
            None,
            Some(
                "Add a Beads comment before claiming work so peers can see the coordination fallback.",
            ),
        ));
        let support_bundle_command = "ee support bundle --workspace . --redacted --dry-run --json";
        actions.push(agent_mail_fallback_action(
            "support_bundle",
            "Plan a redacted support bundle so the storage class and reason can be triaged without raw paths or page offsets.",
            Some(support_bundle_command.to_owned()),
            Some(work_packet_command_action(
                "agent_mail_support_bundle",
                support_bundle_command,
                &[
                    "ee",
                    "support",
                    "bundle",
                    "--workspace",
                    ".",
                    "--redacted",
                    "--dry-run",
                    "--json",
                ],
                false,
                "ee_cli",
                "after_semantic_readiness_failure",
                "Plan bounded support diagnostics without shell evaluation.",
            )),
            None,
        ));
    }
    actions.sort();
    actions.dedup();
    actions
}

fn agent_mail_fallback_action(
    kind: &'static str,
    summary: &'static str,
    command: Option<String>,
    command_action: Option<SwarmWorkPacketCommandAction>,
    manual_step: Option<&'static str>,
) -> SwarmWorkPacketAgentMailFallbackAction {
    let repair_safety = command.as_deref().map_or_else(
        || manual_agent_mail_fallback_repair_safety(kind),
        work_packet_command_repair_safety,
    );
    SwarmWorkPacketAgentMailFallbackAction {
        kind,
        summary,
        command,
        command_action,
        manual_step,
        repair_safety,
    }
}

fn work_packet_command_repair_safety(command: &str) -> SwarmWorkPacketRepairSafety {
    let assessment = classify_repair_command_for_preflight(command);
    SwarmWorkPacketRepairSafety {
        risk_class: assessment.risk_class,
        preflight_command: assessment.preflight_command,
        requires_human_approval: assessment.requires_human_approval,
        mutates_external_state: assessment.mutates_external_state,
        mutates_tracker_state: assessment.mutates_tracker_state,
        privacy_class: assessment.privacy_class,
        next_action: assessment.next_action.as_str(),
        rule_id: assessment.rule_id,
        source: assessment.source,
        reason_code: assessment.reason_code,
        evidence: assessment.evidence,
        preconditions: assessment.preconditions,
    }
}

fn manual_agent_mail_fallback_repair_safety(kind: &str) -> SwarmWorkPacketRepairSafety {
    let reason_code = match kind {
        "manual_coordination" => "manual_coordination_fallback",
        "retry_later" => "retry_later_fallback",
        "switch_to_static_work" => "static_work_fallback",
        "beads_comment" => "manual_beads_coordination_fallback",
        _ => "manual_only_fallback",
    };
    let mut preconditions = vec!["no_agent_runnable_command"];
    if kind == "manual_coordination" || kind == "beads_comment" {
        preconditions.push("shared_state_coordination_required");
    }
    SwarmWorkPacketRepairSafety {
        risk_class: "unavailable_or_manual_only",
        preflight_command: None,
        requires_human_approval: false,
        mutates_external_state: false,
        mutates_tracker_state: false,
        privacy_class: "no_command",
        next_action: "manual_only",
        rule_id: "repair_safety:unavailable_or_manual_only",
        source: "work_packet_manual_fallback",
        reason_code,
        evidence: vec!["agent_mail_fallback_without_command"],
        preconditions,
    }
}

fn work_packet_rch_proof_posture(
    snapshot: &SwarmNextActionSnapshot,
    degraded: &[SwarmWorkPacketDegradation],
) -> SwarmWorkPacketRchProofPosture {
    let known_blockers = work_packet_known_blockers(&snapshot.verification.verifier_evidence);
    let mut blocker_codes = degraded
        .iter()
        .filter(|degradation| degradation.source == "rch")
        .map(|degradation| degradation.code.clone())
        .collect::<BTreeSet<_>>();
    for evidence in &snapshot.verification.verifier_evidence {
        blocker_codes.extend(normalized_rch_blocker_codes_from_evidence(evidence));
    }
    let blocker_codes = blocker_codes.into_iter().collect::<Vec<_>>();
    let topology_blocked = blocker_codes
        .iter()
        .any(|code| code == "rch_worker_topology_blocked");
    let remote_only_required = snapshot.verification.remote_only_required
        || known_blockers.iter().any(|blocker| blocker.remote_required);
    let local_fallback_prevented = remote_only_required
        || blocker_codes
            .iter()
            .any(|code| code == "rch_remote_required_fallback_prevented")
        || snapshot
            .verification
            .verifier_evidence
            .iter()
            .any(|evidence| evidence.local_fallback_refused);
    let source_enabled = snapshot.verification.rch_source_enabled
        || !snapshot.verification.verifier_evidence.is_empty();
    let verifier_blocks_cargo = snapshot
        .verification
        .verifier_evidence
        .iter()
        .any(verifier_evidence_is_environment_blocked);
    let posture = if !source_enabled {
        "unavailable"
    } else if topology_blocked {
        "topology_blocked"
    } else if snapshot.verification.remote_only_safe == Some(true) {
        "remote_ready"
    } else if snapshot.verification.remote_only_safe == Some(false)
        || snapshot.verification.head_of_line_blocked() == Some(true)
    {
        "degraded_capacity"
    } else {
        "unknown"
    };
    let retry_after =
        work_packet_retry_after(&snapshot.verification.verifier_evidence, &known_blockers);
    SwarmWorkPacketRchProofPosture {
        source_enabled,
        remote_only_required,
        posture,
        healthy_worker_count: snapshot.verification.healthy_worker_count,
        safe_to_launch_cargo_verification: if verifier_blocks_cargo {
            Some(false)
        } else {
            snapshot
                .compile_health
                .safe_to_launch_rch
                .and_then(|compile_safe| {
                    let remote_safe = snapshot.verification.remote_only_safe;
                    if remote_only_required {
                        remote_safe.map(|remote_safe| compile_safe && remote_safe)
                    } else {
                        remote_safe
                            .map(|remote_safe| compile_safe && remote_safe)
                            .or(Some(compile_safe))
                    }
                })
        },
        local_fallback_prevented,
        blocker_codes,
        known_blockers,
        retry_after,
    }
}

fn work_packet_known_blockers(
    verifier_evidence: &[SwarmNextActionRecentFirstError],
) -> Vec<SwarmWorkPacketKnownBlocker> {
    let mut known_blockers = verifier_evidence
        .iter()
        .filter_map(|evidence| evidence.known_blocker.clone())
        .collect::<Vec<_>>();
    known_blockers.sort();
    known_blockers.dedup();
    known_blockers
}

fn normalized_rch_blocker_codes_from_evidence(
    evidence: &SwarmNextActionRecentFirstError,
) -> Vec<String> {
    let mut codes = BTreeSet::new();
    if evidence.error_codes.iter().any(|code| code == "RCH-E327")
        || evidence
            .degraded_codes
            .iter()
            .any(|code| code == "rch_verify_topology_blocked")
    {
        codes.insert("rch_worker_topology_blocked".to_owned());
    }
    if evidence.local_fallback_refused
        || evidence
            .degraded_codes
            .iter()
            .any(|code| code == "rch_verify_local_fallback_refused")
    {
        codes.insert("rch_remote_required_fallback_prevented".to_owned());
    }
    codes.into_iter().collect()
}

fn work_packet_retry_after(
    verifier_evidence: &[SwarmNextActionRecentFirstError],
    known_blockers: &[SwarmWorkPacketKnownBlocker],
) -> Option<String> {
    let mut retry_after = verifier_evidence
        .iter()
        .filter_map(|evidence| evidence.retry_after.clone())
        .chain(
            known_blockers
                .iter()
                .filter_map(|blocker| blocker.retry_after.clone()),
        )
        .collect::<Vec<_>>();
    retry_after.sort();
    retry_after.dedup();
    retry_after.into_iter().next()
}

fn work_packet_verification(
    snapshot: &SwarmNextActionSnapshot,
    rch: &SwarmWorkPacketRchProofPosture,
) -> SwarmWorkPacketVerification {
    let mut required_commands = Vec::new();
    if rch.remote_only_required {
        let when = if rch.safe_to_launch_cargo_verification == Some(false) {
            "only_after_rch_remote_workers_recover"
        } else {
            "after_substantive_rust_changes"
        };
        required_commands.push(SwarmWorkPacketVerificationCommand {
            command_id: "cargo_check_all_targets",
            command_template: "RCH_REQUIRE_REMOTE=1 scripts/rch_verify.sh -- cargo check --all-targets"
                .to_owned(),
            command_action: work_packet_command_action(
                "cargo_check_all_targets",
                "RCH_REQUIRE_REMOTE=1 scripts/rch_verify.sh -- cargo check --all-targets",
                &[
                    "env",
                    "RCH_REQUIRE_REMOTE=1",
                    "scripts/rch_verify.sh",
                    "--",
                    "cargo",
                    "check",
                    "--all-targets",
                ],
                true,
                "rch",
                when,
                "Run remote-only Cargo verification through the project RCH wrapper.",
            ),
            required_substrate: "rch",
            when,
            last_outcome: work_packet_rch_last_outcome(snapshot),
            last_command_hash: work_packet_rch_last_command_hash(snapshot),
        });
    }
    let mut static_checks = vec![SwarmWorkPacketVerificationCommand {
        command_id: "diff_check",
        command_template: "git diff --check".to_owned(),
        command_action: work_packet_command_action(
            "diff_check",
            "git diff --check",
            &["git", "diff", "--check"],
            false,
            "git",
            "before_closeout",
            "Reject whitespace errors before preparing a closeout commit.",
        ),
        required_substrate: "static_local",
        when: "before_closeout",
        last_outcome: "not_run",
        last_command_hash: None,
    }];
    if !snapshot.checkout.dirty_paths.is_empty() {
        static_checks.push(SwarmWorkPacketVerificationCommand {
            command_id: "dirty_path_review",
            command_template: "git status --short --branch".to_owned(),
            command_action: work_packet_command_action(
                "dirty_path_review",
                "git status --short --branch",
                &["git", "status", "--short", "--branch"],
                false,
                "git",
                "before_claim_or_closeout",
                "Review shared-checkout dirt before claiming or closing work.",
            ),
            required_substrate: "static_local",
            when: "before_claim_or_closeout",
            last_outcome: "not_run",
            last_command_hash: None,
        });
    }
    static_checks.sort();
    static_checks.dedup();
    SwarmWorkPacketVerification {
        required_commands,
        static_checks,
        closeout_evidence_required: true,
    }
}

fn work_packet_command_action(
    command_id: &'static str,
    display_command: impl Into<String>,
    argv: &[&str],
    mutates_state: bool,
    required_substrate: &'static str,
    when: &'static str,
    rationale: &'static str,
) -> SwarmWorkPacketCommandAction {
    SwarmWorkPacketCommandAction {
        command_id,
        display_command: display_command.into(),
        argv: argv.iter().map(|part| (*part).to_owned()).collect(),
        shell_required: false,
        copy_safety: "safe_structured_argv",
        mutates_state,
        required_substrate,
        when,
        rationale,
    }
}

fn work_packet_rch_last_outcome(snapshot: &SwarmNextActionSnapshot) -> &'static str {
    if snapshot
        .verification
        .verifier_evidence
        .iter()
        .any(verifier_evidence_is_environment_blocked)
    {
        "environment_blocked"
    } else {
        "not_run"
    }
}

fn work_packet_rch_last_command_hash(snapshot: &SwarmNextActionSnapshot) -> Option<String> {
    snapshot
        .verification
        .verifier_evidence
        .iter()
        .filter_map(|evidence| evidence.command_hash.clone())
        .min()
}

fn work_packet_source_provenance(brief: &SwarmBriefReport) -> Vec<SwarmWorkPacketSourceProvenance> {
    let mut provenance = brief
        .sources
        .iter()
        .map(|source| {
            let source_label = work_packet_source_label(source.source.as_str());
            let degraded_codes = source
                .degraded
                .iter()
                .map(|degradation| degradation.code.as_str())
                .collect::<Vec<_>>()
                .join(",");
            let freshness_state = source.freshness.state;
            let digest_input = format!(
                "{}:{}:{}:{}:{}",
                source_label,
                source.status.as_str(),
                freshness_state,
                source.item_count,
                degraded_codes
            );
            let digest_hex = blake3::hash(digest_input.as_bytes()).to_hex().to_string();
            SwarmWorkPacketSourceProvenance {
                source: source_label,
                collector: match source.source {
                    SwarmBriefSourceKind::Bv => "swarm next-action",
                    _ => "swarm brief",
                },
                status: work_packet_source_status(source.status.as_str(), freshness_state),
                freshness: Some(freshness_state.to_owned()),
                digest: Some(format!("blake3:{}", &digest_hex[..16])),
                redaction: match source.source {
                    SwarmBriefSourceKind::AgentMail => "counts_subjects_no_bodies",
                    SwarmBriefSourceKind::Git => "path_patterns_statuses",
                    SwarmBriefSourceKind::Rch => "counts_worker_labels_no_raw_logs",
                    _ => "ids_statuses_counts",
                },
            }
        })
        .collect::<Vec<_>>();
    provenance.sort();
    provenance.dedup();
    provenance
}

fn work_packet_source_status(status: &str, freshness_state: &str) -> &'static str {
    // bd-2z5ly.5: when a source is reachable but its freshness signal says the
    // snapshot is stale (e.g. beads JSONL/DB drift), surface it as "stale" so
    // agents can distinguish "evidence existed but is past TTL" from harder
    // "degraded" signals that mean the collector itself misbehaved. Hard
    // states (unavailable, skipped/not_configured) take precedence.
    match status {
        "unavailable" => "unavailable",
        "skipped" | "not_configured" => "skipped",
        "ready" if freshness_state == "stale" => "stale",
        "ready" => "fresh",
        "degraded" if freshness_state == "stale" => "stale",
        "degraded" => "degraded",
        _ => "degraded",
    }
}

fn work_packet_source_label(source: &str) -> String {
    source.replace('_', "-")
}

fn work_packet_recommended_action(
    snapshot: &SwarmNextActionSnapshot,
    candidates: &[SwarmWorkPacketCandidate],
    agent_mail: &SwarmWorkPacketAgentMail,
    rch: &SwarmWorkPacketRchProofPosture,
    tracker_integrity: &BeadsIntegrityReport,
) -> SwarmWorkPacketRecommendedAction {
    let cards = snapshot.recommendation_cards();
    let selected_card = cards.first();
    let selected_candidate = selected_card
        .and_then(|card| card.candidate_id.as_deref())
        .and_then(|id| candidates.iter().find(|candidate| candidate.id == id))
        .or_else(|| candidates.first());
    let mut reasons = selected_card.map_or_else(Vec::new, |card| card.do_not_take_because.clone());
    if reasons.is_empty() {
        reasons.extend(
            selected_card
                .map(|card| {
                    card.score_inputs
                        .iter()
                        .map(|input| input.name.to_owned())
                        .collect()
                })
                .unwrap_or_else(|| vec!["no_candidate_evidence".to_owned()]),
        );
    }
    if let Some(rch_reason) = work_packet_rch_remote_verification_reason(rch) {
        reasons.push(rch_reason.to_owned());
    }
    if agent_mail_blocks_claim(agent_mail) {
        if agent_mail.status == "semantic_readiness_failed" {
            reasons.push(AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE.to_owned());
            reasons.push("green_transport_does_not_imply_authoritative_reads".to_owned());
        }
        if agent_mail.reservation_authoritative != Some(true) {
            reasons.push("reservation_evidence_not_authoritative".to_owned());
        }
        if agent_mail.inbox_authoritative != Some(true) {
            reasons.push("inbox_evidence_not_authoritative".to_owned());
        }
    }
    if !tracker_integrity.br_reads_authoritative {
        reasons.push(format!(
            "beads_tracker_not_authoritative:{}",
            beads_integrity_health_label(tracker_integrity.health)
        ));
    }
    reasons.sort();
    reasons.dedup();

    let mut proof_obligations = selected_card.map_or_else(
        || vec!["repair_degraded_sources_before_claim".to_owned()],
        |card| card.proof_obligations.clone(),
    );
    if work_packet_rch_remote_verification_reason(rch).is_some() {
        proof_obligations.push("do_not_run_local_cargo_fallback".to_owned());
    }
    if rch.remote_only_required && rch.safe_to_launch_cargo_verification != Some(true) {
        proof_obligations.push("collect_rch_status_before_claim".to_owned());
    }
    if agent_mail_blocks_claim(agent_mail) {
        proof_obligations.push("do_not_treat_zero_inbox_count_as_no_peer_messages".to_owned());
        proof_obligations.push("do_not_treat_zero_reservation_count_as_no_conflict".to_owned());
        if agent_mail.status == "semantic_readiness_failed" {
            proof_obligations
                .push("do_not_treat_green_health_level_as_coordination_authority".to_owned());
            proof_obligations
                .push("record_agent_mail_semantic_readiness_failure_in_beads".to_owned());
        }
    }
    if !tracker_integrity.br_reads_authoritative {
        proof_obligations.push("repair_beads_tracker_before_claim".to_owned());
    }
    proof_obligations.sort();
    proof_obligations.dedup();

    let candidate_id = selected_candidate.map(|candidate| candidate.id.clone());
    let suggested_command_actions = work_packet_suggested_command_actions(
        candidate_id.as_deref(),
        selected_candidate.map(|candidate| candidate.decision),
        agent_mail,
        rch,
        tracker_integrity,
    );
    let suggested_commands = work_packet_display_commands(&suggested_command_actions);
    SwarmWorkPacketRecommendedAction {
        action: work_packet_action(
            selected_card.map(|card| card.decision),
            selected_candidate.map(|candidate| candidate.decision),
            agent_mail,
            rch,
            tracker_integrity,
        ),
        confidence: selected_card.map_or("low", |card| card.confidence),
        safe_to_claim: selected_candidate.map(|candidate| {
            candidate.decision == "safe_to_claim"
                && !agent_mail_blocks_claim(agent_mail)
                && work_packet_rch_allows_claim(rch)
                && tracker_integrity.br_reads_authoritative
        }),
        suggested_commands,
        suggested_command_actions,
        candidate_id,
        reasons,
        proof_obligations,
    }
}

fn work_packet_action(
    card_decision: Option<&'static str>,
    candidate_decision: Option<&'static str>,
    agent_mail: &SwarmWorkPacketAgentMail,
    rch: &SwarmWorkPacketRchProofPosture,
    tracker_integrity: &BeadsIntegrityReport,
) -> &'static str {
    if agent_mail.status == "semantic_readiness_failed" {
        return "prefer_static_docs_work";
    }
    if agent_mail_blocks_claim(agent_mail) {
        return "coordinate_before_claim";
    }
    if !tracker_integrity.br_reads_authoritative {
        return if tracker_integrity.requires_candidate_downgrade {
            "blocked_no_action"
        } else {
            "coordinate_before_claim"
        };
    }
    if work_packet_rch_remote_verification_reason(rch).is_some() {
        return "prefer_static_docs_work";
    }
    match candidate_decision {
        Some("safe_to_claim") => "inspect_and_claim",
        Some("stale_but_reclaimable") => "reopen_stale_work",
        Some(
            "already_owned"
            | "unsafe_due_to_conflict"
            | "stale_review"
            | "coordinate_first"
            | "stale_or_advisory",
        ) => "coordinate_before_claim",
        Some(
            "blocked_by_dependency"
            | "blocked_by_verification"
            | "external_state_required"
            | "release_operator_required"
            | "rollup_only"
            | "blocked_rollup"
            | "blocked"
            | "skip",
        ) => "blocked_no_action",
        _ => match card_decision {
            Some("new_bead_recommended" | "refine_existing_bead") => "inspect_and_claim",
            Some("blocked_by_owner" | "duplicate_rejected" | "reuse_recent_evidence") => {
                "coordinate_before_claim"
            }
            Some("no_action_recommended") => "blocked_no_action",
            _ => "blocked_no_action",
        },
    }
}

fn agent_mail_blocks_claim(agent_mail: &SwarmWorkPacketAgentMail) -> bool {
    agent_mail.status == "semantic_readiness_failed"
}

fn work_packet_rch_remote_verification_reason(
    rch: &SwarmWorkPacketRchProofPosture,
) -> Option<&'static str> {
    if rch.safe_to_launch_cargo_verification == Some(false) {
        Some("rch_remote_verification_blocked")
    } else if rch.remote_only_required && rch.safe_to_launch_cargo_verification != Some(true) {
        Some("rch_remote_verification_required")
    } else {
        None
    }
}

fn work_packet_rch_allows_claim(rch: &SwarmWorkPacketRchProofPosture) -> bool {
    work_packet_rch_remote_verification_reason(rch).is_none()
}

fn work_packet_display_commands(actions: &[SwarmWorkPacketCommandAction]) -> Vec<String> {
    actions
        .iter()
        .map(|action| action.display_command.clone())
        .collect()
}

fn sort_work_packet_command_actions(actions: &mut Vec<SwarmWorkPacketCommandAction>) {
    actions.sort_by(|left, right| {
        left.display_command
            .cmp(&right.display_command)
            .then_with(|| left.command_id.cmp(right.command_id))
            .then_with(|| left.argv.cmp(&right.argv))
    });
    actions.dedup();
}

fn work_packet_suggested_command_actions(
    candidate_id: Option<&str>,
    candidate_decision: Option<&str>,
    agent_mail: &SwarmWorkPacketAgentMail,
    rch: &SwarmWorkPacketRchProofPosture,
    tracker_integrity: &BeadsIntegrityReport,
) -> Vec<SwarmWorkPacketCommandAction> {
    let mut actions = Vec::new();
    if let Some(candidate_id) = candidate_id {
        if tracker_integrity.br_reads_authoritative {
            actions.push(work_packet_command_action(
                "bead_show_candidate",
                format!("br show {candidate_id} --json"),
                &["br", "show", candidate_id, "--json"],
                false,
                "beads",
                "before_claim",
                "Inspect the selected bead before deciding whether to claim it.",
            ));
        } else {
            actions.push(work_packet_command_action(
                "bead_show_candidate_stale_safe",
                format!("br --no-auto-import --allow-stale show {candidate_id} --json"),
                &[
                    "br",
                    "--no-auto-import",
                    "--allow-stale",
                    "show",
                    candidate_id,
                    "--json",
                ],
                false,
                "beads",
                "before_claim",
                "Inspect the selected bead without mutating a stale tracker index.",
            ));
        }
        if tracker_integrity.br_reads_authoritative
            && work_packet_rch_allows_claim(rch)
            && !agent_mail_blocks_claim(agent_mail)
            && candidate_decision == Some("safe_to_claim")
        {
            actions.push(work_packet_command_action(
                "bead_claim_candidate",
                format!("br update {candidate_id} --status in_progress --json"),
                &[
                    "br",
                    "update",
                    candidate_id,
                    "--status",
                    "in_progress",
                    "--json",
                ],
                true,
                "beads",
                "after_inspection",
                "Claim the selected bead after safety checks pass.",
            ));
        }
        if agent_mail.status == "semantic_readiness_failed" {
            actions.push(work_packet_command_action(
                "bead_comment_agent_mail_semantic_readiness",
                format!(
                    "br comments add {candidate_id} --message 'agent_mail semantic_readiness=fail; coordinating via beads until repair'"
                ),
                &[
                    "br",
                    "comments",
                    "add",
                    candidate_id,
                    "--message",
                    "agent_mail semantic_readiness=fail; coordinating via beads until repair",
                ],
                true,
                "beads",
                "before_claim",
                "Record that Beads is the coordination fallback while Agent Mail is not authoritative.",
            ));
        }
    }
    if !tracker_integrity.br_reads_authoritative {
        actions.push(work_packet_command_action(
            "beads_doctor_no_db",
            "br doctor --json --no-db",
            &["br", "doctor", "--json", "--no-db"],
            false,
            "beads",
            "before_claim",
            "Inspect tracker health without relying on the stale database.",
        ));
    }
    actions.push(work_packet_command_action(
        "swarm_brief_refresh",
        "ee swarm brief --workspace . --json",
        &["ee", "swarm", "brief", "--workspace", ".", "--json"],
        false,
        "ee",
        "before_claim_or_closeout",
        "Refresh the read-only swarm input snapshot before acting.",
    ));
    sort_work_packet_command_actions(&mut actions);
    actions
}

fn beads_integrity_health_label(health: BeadsIntegrityHealth) -> &'static str {
    match health {
        BeadsIntegrityHealth::Ok => "ok",
        BeadsIntegrityHealth::MergeArtifactsWarn => "merge_artifacts_warn",
        BeadsIntegrityHealth::ExternalChangesPendingImport => "external_changes_pending_import",
        BeadsIntegrityHealth::DbJsonlCountMismatch => "db_jsonl_count_mismatch",
        BeadsIntegrityHealth::JsonlParseError => "jsonl_parse_error",
    }
}

fn work_packet_observed_state_class(
    coordination: &SwarmWorkPacketCoordination,
    rch: &SwarmWorkPacketRchProofPosture,
    degraded: &[SwarmWorkPacketDegradation],
) -> &'static str {
    let agent_mail_degraded = matches!(
        coordination.agent_mail.status,
        "degraded_read_only" | "semantic_readiness_failed" | "unavailable"
    );
    let rch_degraded = matches!(rch.posture, "topology_blocked" | "degraded_capacity");
    if agent_mail_degraded || rch_degraded {
        "degraded_mail_rch_topology"
    } else if coordination.active_claim_count > 0
        || coordination.file_collision_count > 0
        || coordination.dirty_path_count > 0
    {
        "crowded_checkout"
    } else if degraded.is_empty() {
        "healthy_small_repo"
    } else {
        "unknown"
    }
}

#[must_use]
pub fn verifier_evidence_from_json(value: &Value) -> Vec<SwarmNextActionRecentFirstError> {
    let mut evidence = Vec::new();
    collect_verifier_evidence_items(value, &mut evidence);
    let mut successes = Vec::new();
    collect_verifier_success_items(value, &mut successes);
    successes.sort();
    successes.dedup();
    evidence.retain(|item| !verifier_evidence_superseded_by_success(item, &successes));
    evidence.sort();
    evidence.dedup();
    evidence
}

fn verifier_evidence_from_ledger_blocker(
    run: &RchVerifyRunView,
) -> SwarmNextActionRecentFirstError {
    let local_fallback_refused = run
        .degraded_codes
        .iter()
        .any(|code| code == "rch_verify_local_fallback_refused");
    let code = run
        .degraded_codes
        .first()
        .cloned()
        .unwrap_or_else(|| "rch_verify_known_blocker_active".to_owned());
    let known_blocker = run
        .blocker_fingerprint
        .as_ref()
        .map(|fingerprint| SwarmWorkPacketKnownBlocker {
            code,
            fingerprint: fingerprint.clone(),
            command_hash: Some(run.command_hash.clone()),
            message: Some(
                "Active durable verifier-ledger blocker; avoid duplicate RCH until retry_after or an exact-key successful proof clears it."
                    .to_owned(),
            ),
            remediation_bead: run.remediation_bead.clone(),
            retry_after: run.retry_after.clone(),
            remote_required: run.remote_required,
            local_fallback_refused,
            degraded_codes: run.degraded_codes.clone(),
        });
    SwarmNextActionRecentFirstError {
        file: "rch_verify_ledger".to_owned(),
        line: None,
        command_kind: Some(run.command_kind.clone()),
        command: run.command_text.clone(),
        command_hash: Some(run.command_hash.clone()),
        status: Some(run.status.clone()),
        degraded_codes: run.degraded_codes.clone(),
        source_state_hash: Some(run.source_state_hash.clone()),
        created_at: Some(run.created_at.clone()),
        error_codes: Vec::new(),
        remote_required: Some(run.remote_required),
        local_fallback_refused,
        retry_after: run.retry_after.clone(),
        known_blocker,
    }
}

fn collect_verifier_evidence_items(
    value: &Value,
    evidence: &mut Vec<SwarmNextActionRecentFirstError>,
) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_verifier_evidence_items(item, evidence);
            }
        }
        Value::Object(object) => {
            if let Some(item) = verifier_evidence_item(value) {
                evidence.push(item);
            }
            for key in ["runs", "proofs", "entries", "ledger", "items"] {
                if let Some(nested) = object.get(key) {
                    collect_verifier_evidence_items(nested, evidence);
                }
            }
        }
        _ => {}
    }
}

fn verifier_evidence_item(value: &Value) -> Option<SwarmNextActionRecentFirstError> {
    let object = value.as_object()?;
    let first = value.get("first_error").or_else(|| value.get("firstError"));
    let file = string_value_from_keys(
        object,
        &["first_error_file", "firstErrorFile", "file", "path"],
    )
    .or_else(|| {
        first
            .and_then(Value::as_object)
            .and_then(|object| object.get("file").or_else(|| object.get("path")))
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
    .map(|path| normalize_remote_repo_path(&path))
    .unwrap_or_default();
    let degraded_codes = string_array_from_keys(
        object,
        &[
            "degraded_codes",
            "degradedCodes",
            "source_state_degraded_codes",
            "sourceStateDegradedCodes",
            "worker_state_degraded_codes",
            "workerStateDegradedCodes",
        ],
    );
    let error_codes = string_array_from_keys(object, &["error_codes", "errorCodes"]);
    let status = string_value_from_keys(object, &["status", "result", "outcome"]);
    let failure_like = status.as_deref().is_some_and(|status| {
        matches!(
            status,
            "remote_failure"
                | "failed"
                | "failure"
                | "rch_environment_failure"
                | "known_blocker_refused"
                | "environment_blocked"
        )
    }) || !error_codes.is_empty()
        || degraded_codes
            .iter()
            .any(|code| code == "rch_verify_remote_command_failed")
        || degraded_codes_are_environment_blockers(&degraded_codes);
    if !failure_like {
        return None;
    }
    let line = value
        .get("first_error_line")
        .or_else(|| value.get("firstErrorLine"))
        .and_then(Value::as_u64)
        .or_else(|| {
            first
                .and_then(Value::as_object)
                .and_then(|object| object.get("line"))
                .and_then(Value::as_u64)
        });
    let local_fallback_refused = degraded_codes
        .iter()
        .any(|code| code == "rch_verify_local_fallback_refused");
    let remote_required = bool_value_from_keys(object, &["remote_required", "remoteRequired"]);
    let command_hash = string_value_from_keys(object, &["command_hash", "commandHash"]);
    let retry_after = string_value_from_keys(object, &["retry_after", "retryAfter"]);
    let command_kind = string_value_from_keys(object, &["command_kind", "commandKind"]);
    let command = string_value_from_keys(object, &["command_text", "commandText", "command"])
        .or_else(|| command_from_array_field(object, "args"))
        .or_else(|| command_from_array_field(object, "argv"));
    let known_blocker = known_blocker_from_json(
        object,
        command_hash.as_deref(),
        retry_after.as_deref(),
        remote_required,
        local_fallback_refused,
        &degraded_codes,
        &error_codes,
    );
    let source_state_hash =
        string_value_from_keys(object, &["source_state_hash", "sourceStateHash"]);
    let created_at = string_value_from_keys(object, &["created_at", "createdAt"]);
    Some(SwarmNextActionRecentFirstError {
        file,
        line,
        command_kind,
        command,
        command_hash,
        status,
        degraded_codes,
        source_state_hash,
        created_at,
        error_codes,
        remote_required,
        local_fallback_refused,
        retry_after,
        known_blocker,
    })
}

fn collect_verifier_success_items(value: &Value, successes: &mut Vec<VerifierSuccessfulProof>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_verifier_success_items(item, successes);
            }
        }
        Value::Object(object) => {
            if let Some(item) = verifier_success_item(value) {
                successes.push(item);
            }
            for key in ["runs", "proofs", "entries", "ledger", "items"] {
                if let Some(nested) = object.get(key) {
                    collect_verifier_success_items(nested, successes);
                }
            }
        }
        _ => {}
    }
}

fn verifier_success_item(value: &Value) -> Option<VerifierSuccessfulProof> {
    let object = value.as_object()?;
    let status = string_value_from_keys(object, &["status", "result", "outcome"])?;
    if !matches!(
        status.as_str(),
        "passed" | "remote_pass" | "pass_without_remote_marker"
    ) {
        return None;
    }
    Some(VerifierSuccessfulProof {
        command_hash: string_value_from_keys(object, &["command_hash", "commandHash"])?,
        source_state_hash: string_value_from_keys(
            object,
            &["source_state_hash", "sourceStateHash"],
        )?,
        created_at: string_value_from_keys(object, &["created_at", "createdAt"]),
    })
}

fn verifier_evidence_superseded_by_success(
    evidence: &SwarmNextActionRecentFirstError,
    successes: &[VerifierSuccessfulProof],
) -> bool {
    let (Some(command_hash), Some(source_state_hash)) = (
        evidence.command_hash.as_deref(),
        evidence.source_state_hash.as_deref(),
    ) else {
        return false;
    };
    successes.iter().any(|success| {
        success.command_hash == command_hash
            && success.source_state_hash == source_state_hash
            && verifier_success_is_later(
                evidence.created_at.as_deref(),
                success.created_at.as_deref(),
            )
    })
}

fn verifier_success_is_later(
    blocker_created_at: Option<&str>,
    success_created_at: Option<&str>,
) -> bool {
    match (blocker_created_at, success_created_at) {
        (Some(blocker_created_at), Some(success_created_at)) => {
            success_created_at >= blocker_created_at
        }
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn verifier_evidence_is_environment_blocked(evidence: &SwarmNextActionRecentFirstError) -> bool {
    evidence.status.as_deref().is_some_and(|status| {
        matches!(
            status,
            "rch_environment_failure" | "known_blocker_refused" | "environment_blocked"
        )
    }) || evidence.error_codes.iter().any(|code| code == "RCH-E327")
        || degraded_codes_are_environment_blockers(&evidence.degraded_codes)
}

fn degraded_codes_are_environment_blockers(codes: &[String]) -> bool {
    codes.iter().any(|code| {
        matches!(
            code.as_str(),
            "rch_verify_topology_blocked"
                | "rch_verify_local_fallback_refused"
                | "rch_verify_remote_marker_missing"
                | "rch_verify_known_blocker_active"
                | "rch_verify_cargo_path_dependency_version_blocked"
        )
    })
}

fn string_value_from_keys(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .filter_map(|key| value_from_object_or_fields(object, key))
        .find_map(|value| value.as_str().map(str::to_owned))
        .filter(|value| !value.is_empty())
}

fn bool_value_from_keys(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .filter_map(|key| value_from_object_or_fields(object, key))
        .find_map(Value::as_bool)
}

fn value_from_object_or_fields<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Option<&'a Value> {
    object.get(key).or_else(|| {
        object
            .get("fields")
            .and_then(Value::as_object)
            .and_then(|fields| fields.get(key))
    })
}

fn string_array_from_keys(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Vec<String> {
    let mut strings = Vec::new();
    for key in keys {
        let Some(Value::Array(items)) = value_from_object_or_fields(object, key) else {
            continue;
        };
        strings.extend(items.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    strings.sort();
    strings.dedup();
    strings
}

fn command_from_array_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    let value = value_from_object_or_fields(object, key)?;
    let joined = value
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then_some(joined)
}

fn known_blocker_from_json(
    object: &serde_json::Map<String, Value>,
    command_hash: Option<&str>,
    retry_after: Option<&str>,
    remote_required: Option<bool>,
    local_fallback_refused: bool,
    degraded_codes: &[String],
    error_codes: &[String],
) -> Option<SwarmWorkPacketKnownBlocker> {
    let known_blocker = value_from_object_or_fields(object, "known_blocker")
        .or_else(|| value_from_object_or_fields(object, "knownBlocker"))
        .and_then(Value::as_object);
    if known_blocker.is_none()
        && error_codes.is_empty()
        && !local_fallback_refused
        && !degraded_codes_are_environment_blockers(degraded_codes)
    {
        return None;
    }

    let retry_after = known_blocker
        .and_then(|known_blocker| {
            string_value_from_keys(known_blocker, &["retry_after", "retryAfter"])
        })
        .or_else(|| retry_after.map(str::to_owned));
    let command_hash = known_blocker
        .and_then(|known_blocker| {
            string_value_from_keys(known_blocker, &["command_hash", "commandHash"])
        })
        .or_else(|| command_hash.map(str::to_owned));
    let degraded_codes = known_blocker
        .map(|known_blocker| {
            string_array_from_keys(known_blocker, &["degraded_codes", "degradedCodes"])
        })
        .unwrap_or_default()
        .into_iter()
        .chain(degraded_codes.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let code = known_blocker
        .and_then(|known_blocker| {
            string_value_from_keys(known_blocker, &["code", "blocker_kind", "blockerKind"])
        })
        .or_else(|| error_codes.first().cloned())
        .or_else(|| degraded_codes.first().cloned())
        .unwrap_or_else(|| "known_blocker".to_owned());
    let fingerprint = known_blocker
        .and_then(|known_blocker| {
            string_value_from_keys(
                known_blocker,
                &["blocker_fingerprint", "blockerFingerprint", "fingerprint"],
            )
        })
        .or_else(|| string_value_from_keys(object, &["blocker_fingerprint", "blockerFingerprint"]))
        .unwrap_or_else(|| {
            synthesized_known_blocker_fingerprint(
                &code,
                command_hash.as_deref(),
                &degraded_codes,
                error_codes,
            )
        });
    Some(SwarmWorkPacketKnownBlocker {
        code,
        fingerprint,
        command_hash,
        message: known_blocker.and_then(|known_blocker| {
            string_value_from_keys(known_blocker, &["message", "summary"])
        }),
        remediation_bead: known_blocker
            .and_then(|known_blocker| {
                string_value_from_keys(known_blocker, &["remediation_bead", "remediationBead"])
            })
            .or_else(|| string_value_from_keys(object, &["remediation_bead", "remediationBead"]))
            .or_else(|| {
                error_codes
                    .iter()
                    .any(|code| code == "RCH-E327")
                    .then(|| "bd-17c65.10.17.1.2".to_owned())
            }),
        retry_after,
        remote_required: known_blocker
            .and_then(|known_blocker| {
                bool_value_from_keys(known_blocker, &["remote_required", "remoteRequired"])
            })
            .or(remote_required)
            .unwrap_or(local_fallback_refused),
        local_fallback_refused: known_blocker
            .and_then(|known_blocker| {
                bool_value_from_keys(
                    known_blocker,
                    &["local_fallback_refused", "localFallbackRefused"],
                )
            })
            .unwrap_or(local_fallback_refused),
        degraded_codes,
    })
}

fn synthesized_known_blocker_fingerprint(
    code: &str,
    command_hash: Option<&str>,
    degraded_codes: &[String],
    error_codes: &[String],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ee.rch.known_blocker.v1\0");
    hasher.update(code.as_bytes());
    hasher.update(b"\0");
    hasher.update(command_hash.unwrap_or("").as_bytes());
    hasher.update(b"\0");
    hasher.update(degraded_codes.join(",").as_bytes());
    hasher.update(b"\0");
    hasher.update(error_codes.join(",").as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn normalize_remote_repo_path(path: &str) -> String {
    path.strip_prefix("/data/projects/eidetic_engine_cli/")
        .unwrap_or(path)
        .to_owned()
}

impl SwarmNextActionDegradation {
    fn from_brief(degradation: &SwarmBriefDegradation) -> Self {
        Self {
            code: degradation.code.clone(),
            source: degradation.source.as_str().to_owned(),
            severity: degradation.severity,
            message: degradation.message.clone(),
            repair: degradation.repair.clone(),
        }
    }
}

fn candidates_from_brief(
    brief: &SwarmBriefReport,
    blocked_by_compile_health: bool,
) -> Vec<SwarmNextActionCandidate> {
    let mut candidates = Vec::new();
    if let Some(bv) = &brief.bv {
        for pick in &bv.top_picks {
            let bead = brief
                .beads
                .ready
                .iter()
                .chain(brief.beads.in_progress.iter())
                .chain(brief.beads.blocked.iter())
                .find(|bead| bead.id == pick.id);
            candidates.push(SwarmNextActionCandidate {
                id: pick.id.clone(),
                title: pick.title.clone(),
                source: "bv_top_pick",
                score_milli: pick.score_milli,
                status: bead.map_or_else(|| "unknown".to_owned(), |bead| bead.status.clone()),
                priority: bead.and_then(|bead| bead.priority),
                issue_type: bead.and_then(|bead| bead.issue_type.clone()),
                assignee: bead.and_then(|bead| bead.assignee.clone()),
                blocked_by: pick.blocked_by.clone(),
                blocked_by_compile_health,
                action_hint: pick
                    .action_hint
                    .clone()
                    .unwrap_or_else(|| "inspect_and_reserve_before_editing".to_owned()),
            });
        }
    }
    for bead in &brief.beads.ready {
        candidates.push(SwarmNextActionCandidate {
            id: bead.id.clone(),
            title: bead.title.clone(),
            source: "beads_ready",
            score_milli: None,
            status: bead.status.clone(),
            priority: bead.priority,
            issue_type: bead.issue_type.clone(),
            assignee: bead.assignee.clone(),
            blocked_by: Vec::new(),
            blocked_by_compile_health,
            action_hint: "reserve_files_and_start_smallest_useful_slice".to_owned(),
        });
    }
    candidates
}

fn candidate_source_rank(source: &str) -> u8 {
    match source {
        "bv_top_pick" => 0,
        "beads_ready" => 1,
        _ => 2,
    }
}

fn stale_work_proposals_from_brief(
    brief: &SwarmBriefReport,
) -> Vec<SwarmNextActionStaleWorkProposal> {
    let agent_mail_degraded = source_has_degradation(brief, SwarmBriefSourceKind::AgentMail);
    let beads_degraded = source_has_degradation(brief, SwarmBriefSourceKind::Beads);
    let mut proposals = brief
        .beads
        .in_progress
        .iter()
        .map(|bead| stale_work_proposal_for_bead(brief, bead, agent_mail_degraded, beads_degraded))
        .collect::<Vec<_>>();
    proposals.sort();
    proposals.dedup();
    proposals
}

fn source_has_degradation(brief: &SwarmBriefReport, source: SwarmBriefSourceKind) -> bool {
    brief
        .degraded
        .iter()
        .any(|degradation| degradation.source == source)
}

fn stale_work_proposal_for_bead(
    brief: &SwarmBriefReport,
    bead: &SwarmBriefBead,
    agent_mail_degraded: bool,
    beads_degraded: bool,
) -> SwarmNextActionStaleWorkProposal {
    let matching_reservation = matching_reservation_for_in_progress_bead(brief, bead);
    let matching_commit = matching_recent_commit_for_bead(brief, bead);
    let matching_thread = matching_thread_for_bead(brief, bead);
    let blocked_by = bv_blockers_for_bead(brief, bead);

    let mut evidence = BTreeSet::new();
    evidence.insert(format!("status:{}", bead.status));
    evidence.insert(format!("source_bucket:{}", bead.source_bucket));
    if let Some(priority) = bead.priority {
        evidence.insert(format!("priority:{priority}"));
    }
    match &bead.assignee {
        Some(assignee) => {
            evidence.insert(format!("assignee_present:{assignee}"));
        }
        None => {
            evidence.insert("assignee_missing".to_owned());
        }
    }

    if let Some(reservation) = matching_reservation {
        evidence.insert(format!(
            "active_reservation_holder:{}:{}",
            reservation.holder, reservation.path_pattern
        ));
    } else {
        evidence.insert("no_matching_active_reservation".to_owned());
    }

    if let Some(commit) = matching_commit {
        evidence.insert(format!("recent_commit_mentions_bead:{}", commit.hash));
    } else {
        evidence.insert("no_recent_commit_mentions_bead".to_owned());
    }

    if let Some(thread) = matching_thread {
        evidence.insert(format!("mail_thread_mentions_bead:{}", thread.thread_id));
    } else {
        evidence.insert("no_mail_thread_mentions_bead".to_owned());
    }

    if !blocked_by.is_empty() {
        evidence.insert(format!("blocked_by:{}", blocked_by.join(",")));
    }

    let mut caveats = BTreeSet::new();
    if agent_mail_degraded {
        caveats.insert("agent_mail_unavailable_not_stale_evidence".to_owned());
    }
    if beads_degraded {
        caveats.insert("beads_tracker_degraded_timestamps_may_be_stale".to_owned());
    }
    if bead.assignee.is_none() {
        caveats.insert("missing_assignee_reduces_contactability".to_owned());
    }
    if !blocked_by.is_empty() {
        caveats.insert("blocked_dependencies_require_parent_check_before_reopen".to_owned());
    }

    let has_active_signal =
        matching_reservation.is_some() || matching_commit.is_some() || matching_thread.is_some();
    let stale_signal_count = [
        matching_reservation.is_none(),
        matching_commit.is_none(),
        matching_thread.is_none(),
        bead.assignee.is_none(),
    ]
    .into_iter()
    .filter(|signal| *signal)
    .count();

    let (decision, confidence) = if has_active_signal {
        ("leaveAloneActive", "high")
    } else if agent_mail_degraded || beads_degraded || !blocked_by.is_empty() {
        ("contactSuggested", "low")
    } else if stale_signal_count >= 3 {
        ("reopenSuggested", "medium")
    } else {
        ("contactSuggested", "medium")
    };

    SwarmNextActionStaleWorkProposal {
        bead_id: bead.id.clone(),
        title: bead.title.clone(),
        assignee: bead.assignee.clone(),
        decision,
        confidence,
        evidence: evidence.into_iter().collect(),
        caveats: caveats.into_iter().collect(),
        suggested_commands: stale_work_suggested_commands(bead, decision),
    }
}

fn matching_reservation_for_in_progress_bead<'a>(
    brief: &'a SwarmBriefReport,
    bead: &SwarmBriefBead,
) -> Option<&'a SwarmBriefFileReservation> {
    let assignee = bead.assignee.as_deref()?;
    brief
        .file_reservations
        .iter()
        .find(|reservation| reservation.exclusive && reservation.holder == assignee)
}

fn matching_recent_commit_for_bead<'a>(
    brief: &'a SwarmBriefReport,
    bead: &SwarmBriefBead,
) -> Option<&'a SwarmBriefCommit> {
    brief
        .recent_commits
        .iter()
        .find(|commit| text_mentions_bead(&commit.subject, &bead.id))
}

fn matching_thread_for_bead<'a>(
    brief: &'a SwarmBriefReport,
    bead: &SwarmBriefBead,
) -> Option<&'a SwarmBriefThreadSummary> {
    brief.threads.iter().find(|thread| {
        text_mentions_bead(&thread.thread_id, &bead.id)
            || thread
                .subject
                .as_deref()
                .is_some_and(|subject| text_mentions_bead(subject, &bead.id))
    })
}

fn bv_blockers_for_bead(brief: &SwarmBriefReport, bead: &SwarmBriefBead) -> Vec<String> {
    let mut blockers = brief
        .bv
        .as_ref()
        .and_then(|summary| summary.top_picks.iter().find(|pick| pick.id == bead.id))
        .map_or_else(Vec::new, |pick| pick.blocked_by.clone());
    blockers.sort();
    blockers.dedup();
    blockers
}

fn text_mentions_bead(text: &str, bead_id: &str) -> bool {
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '.'))
        .any(|token| token == bead_id)
}

fn stale_work_suggested_commands(bead: &SwarmBriefBead, decision: &'static str) -> Vec<String> {
    match decision {
        "reopenSuggested" => vec![
            format!("br show {} --json", bead.id),
            format!("br update {} --status open --json", bead.id),
        ],
        "contactSuggested" => vec![
            format!("br show {} --json", bead.id),
            format!("br update {} --status in_progress --json", bead.id),
        ],
        "leaveAloneActive" => vec![format!("br show {} --json", bead.id)],
        _ => Vec::new(),
    }
}

fn recommendation_cards_from_snapshot(
    snapshot: &SwarmNextActionSnapshot,
) -> Vec<SwarmNextActionRecommendationCard> {
    if snapshot.candidates.is_empty() {
        return no_action_recommendation_cards(snapshot);
    }

    let mut candidate_counts = BTreeMap::<&str, usize>::new();
    let mut candidate_title_counts = BTreeMap::<String, usize>::new();
    for candidate in &snapshot.candidates {
        *candidate_counts.entry(candidate.id.as_str()).or_default() += 1;
        *candidate_title_counts
            .entry(candidate_title_overlap_key(&candidate.title))
            .or_default() += 1;
    }

    let caveats = recommendation_evidence_caveats(snapshot);
    let has_compile_owner_blocker = snapshot
        .compile_health
        .blockers
        .iter()
        .any(|blocker| blocker.owner_agent.is_some());
    let reusable_verifier_evidence = reusable_verifier_evidence_for_snapshot(snapshot);
    let mut ranked_cards = snapshot
        .candidates
        .iter()
        .map(|candidate| {
            let duplicate_reason = duplicate_reason_for_candidate(
                candidate,
                &candidate_counts,
                &candidate_title_counts,
            );
            let duplicate = duplicate_reason.is_some();
            let blocked_by_owner = candidate.assignee.is_some()
                || (candidate.blocked_by_compile_health && has_compile_owner_blocker);
            let has_reusable_verifier_evidence =
                candidate.blocked_by_compile_health && !reusable_verifier_evidence.is_empty();
            let rank_milli =
                recommendation_rank_milli(candidate, duplicate, blocked_by_owner, &caveats);
            let card = recommendation_card_for_candidate(
                candidate,
                duplicate_reason,
                blocked_by_owner,
                has_reusable_verifier_evidence,
                rank_milli,
                &caveats,
                &reusable_verifier_evidence,
            );
            (rank_milli, card)
        })
        .collect::<Vec<_>>();
    ranked_cards.sort_by(|(left_rank, left), (right_rank, right)| {
        recommendation_card_sort_key(*right_rank, right)
            .cmp(&recommendation_card_sort_key(*left_rank, left))
    });
    let mut cards = ranked_cards
        .into_iter()
        .map(|(_, card)| card)
        .collect::<Vec<_>>();
    cards.dedup();
    cards
}

fn candidate_title_overlap_key(title: &str) -> String {
    title
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.'))
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn duplicate_reason_for_candidate(
    candidate: &SwarmNextActionCandidate,
    candidate_counts: &BTreeMap<&str, usize>,
    candidate_title_counts: &BTreeMap<String, usize>,
) -> Option<&'static str> {
    if candidate_counts
        .get(candidate.id.as_str())
        .copied()
        .unwrap_or(1)
        > 1
    {
        return Some("candidate_id_already_present");
    }
    if candidate_title_counts
        .get(&candidate_title_overlap_key(&candidate.title))
        .copied()
        .unwrap_or(1)
        > 1
    {
        return Some("candidate_title_already_present");
    }
    None
}

fn recommendation_card_sort_key(
    rank_milli: i64,
    card: &SwarmNextActionRecommendationCard,
) -> (
    i64,
    i64,
    std::cmp::Reverse<String>,
    std::cmp::Reverse<String>,
) {
    (
        rank_milli,
        card.score_inputs
            .iter()
            .find(|input| input.name == "priority")
            .and_then(|input| input.value.parse::<i64>().ok())
            .map_or(0, |priority| -priority),
        std::cmp::Reverse(card.candidate_id.clone().unwrap_or_default()),
        std::cmp::Reverse(card.candidate_summary.clone()),
    )
}

fn no_action_recommendation_cards(
    snapshot: &SwarmNextActionSnapshot,
) -> Vec<SwarmNextActionRecommendationCard> {
    if snapshot.degraded.is_empty() {
        return Vec::new();
    }
    vec![SwarmNextActionRecommendationCard {
        card_id: "no_action_recommended:evidence_unavailable".to_owned(),
        candidate_id: None,
        candidate_source: "evidence_providers",
        candidate_summary:
            "No safe recommendation because selected evidence providers are degraded.".to_owned(),
        decision: "no_action_recommended",
        confidence: "low",
        score_inputs: Vec::new(),
        suggested_reservations: Vec::new(),
        do_not_take_because: vec!["selected_evidence_providers_are_degraded".to_owned()],
        overlap: SwarmNextActionOverlapDecision {
            decision: "no_action_recommended",
            queries: Vec::new(),
            matched_existing_beads: Vec::new(),
            rejected_duplicate_reason: None,
            selected_relation: "none",
        },
        proof_obligations: vec!["repair_degraded_sources_before_creating_tracker_work".to_owned()],
        evidence_caveats: recommendation_evidence_caveats(snapshot),
        fallback_decision: Some("repair_evidence_providers"),
    }]
}

fn recommendation_card_for_candidate(
    candidate: &SwarmNextActionCandidate,
    duplicate_reason: Option<&'static str>,
    blocked_by_owner: bool,
    has_reusable_verifier_evidence: bool,
    rank_milli: i64,
    evidence_caveats: &[String],
    reusable_verifier_evidence: &[String],
) -> SwarmNextActionRecommendationCard {
    let decision = if duplicate_reason.is_some() {
        "duplicate_rejected"
    } else if blocked_by_owner {
        "blocked_by_owner"
    } else if candidate_is_rollup(candidate) {
        "blocked_rollup"
    } else if has_reusable_verifier_evidence {
        "reuse_recent_evidence"
    } else if candidate.status == "unknown" {
        "new_bead_recommended"
    } else {
        "refine_existing_bead"
    };
    let fallback_decision = match decision {
        "duplicate_rejected" => Some("refine_existing_bead"),
        "blocked_by_owner" => Some("message_owner_before_editing"),
        "blocked_rollup" => Some("choose_concrete_child_bead"),
        "reuse_recent_evidence" => Some("prefer_static_or_non_cargo_work"),
        _ => None,
    };

    SwarmNextActionRecommendationCard {
        card_id: format!("{decision}:{}", candidate.id),
        candidate_id: Some(candidate.id.clone()),
        candidate_source: candidate.source,
        candidate_summary: candidate.title.clone(),
        decision,
        confidence: recommendation_confidence(candidate, decision, evidence_caveats),
        score_inputs: recommendation_score_inputs(candidate, rank_milli),
        suggested_reservations: suggested_reservations_for_candidate(candidate, decision),
        do_not_take_because: do_not_take_reasons_for_candidate(
            candidate,
            decision,
            duplicate_reason.is_some(),
            blocked_by_owner,
            evidence_caveats,
            reusable_verifier_evidence,
        ),
        overlap: overlap_decision_for_candidate(candidate, decision, duplicate_reason),
        proof_obligations: recommendation_proof_obligations(candidate, decision),
        evidence_caveats: evidence_caveats.to_vec(),
        fallback_decision,
    }
}

fn overlap_decision_for_candidate(
    candidate: &SwarmNextActionCandidate,
    decision: &'static str,
    duplicate_reason: Option<&'static str>,
) -> SwarmNextActionOverlapDecision {
    let mut matched_existing_beads = Vec::new();
    if candidate.status != "unknown" {
        matched_existing_beads.push(candidate.id.clone());
    }
    matched_existing_beads.extend(candidate.blocked_by.iter().cloned());
    matched_existing_beads.sort();
    matched_existing_beads.dedup();

    let mut queries = vec![
        format!("bead_id:{}", candidate.id),
        format!("source:{}", candidate.source),
        format!("title:{}", candidate.title),
    ];
    queries.sort();

    SwarmNextActionOverlapDecision {
        decision,
        queries,
        matched_existing_beads,
        rejected_duplicate_reason: duplicate_reason,
        selected_relation: match decision {
            "new_bead_recommended" => "new_child",
            "duplicate_rejected" | "refine_existing_bead" => "existing_bead",
            "blocked_by_owner" => "owner_coordination_required",
            "blocked_rollup" => "rollup_not_claimable",
            _ => "none",
        },
    }
}

fn candidate_is_rollup(candidate: &SwarmNextActionCandidate) -> bool {
    if let Some(issue_type) = &candidate.issue_type {
        let normalized = issue_type.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "epic" | "theme" | "rollup") {
            return true;
        }
    }
    let title = candidate.title.to_ascii_lowercase();
    title.starts_with("[theme]") || title.starts_with("[epic]")
}

fn recommendation_rank_milli(
    candidate: &SwarmNextActionCandidate,
    duplicate: bool,
    blocked_by_owner: bool,
    evidence_caveats: &[String],
) -> i64 {
    let source_bonus = match candidate.source {
        "bv_top_pick" => 250,
        "beads_ready" => 150,
        _ => 0,
    };
    let priority_bonus = candidate
        .priority
        .map_or(0, |priority| (6_i64.saturating_sub(priority)).max(0) * 75);
    let mut score = i64::from(candidate.score_milli.unwrap_or(500)) + source_bonus + priority_bonus;

    score -= i64::try_from(candidate.blocked_by.len()).unwrap_or(i64::MAX / 100) * 100;
    if candidate.blocked_by_compile_health {
        score -= 350;
    }
    if candidate.assignee.is_some() {
        score -= 400;
    }
    if duplicate {
        score -= 800;
    }
    if blocked_by_owner {
        score -= 500;
    }
    if candidate_is_rollup(candidate) {
        score -= 1_000;
    }
    score -= i64::try_from(evidence_caveats.len()).unwrap_or(i64::MAX / 25) * 25;
    score
}

fn recommendation_score_inputs(
    candidate: &SwarmNextActionCandidate,
    rank_milli: i64,
) -> Vec<SwarmNextActionScoreInput> {
    let mut inputs = vec![
        SwarmNextActionScoreInput {
            name: "rank_milli",
            value: rank_milli.to_string(),
        },
        SwarmNextActionScoreInput {
            name: "source_rank",
            value: candidate_source_rank(candidate.source).to_string(),
        },
        SwarmNextActionScoreInput {
            name: "status",
            value: candidate.status.clone(),
        },
        SwarmNextActionScoreInput {
            name: "blocked_by_compile_health",
            value: candidate.blocked_by_compile_health.to_string(),
        },
        SwarmNextActionScoreInput {
            name: "blocked_by_count",
            value: candidate.blocked_by.len().to_string(),
        },
    ];
    if let Some(score_milli) = candidate.score_milli {
        inputs.push(SwarmNextActionScoreInput {
            name: "bv_score_milli",
            value: score_milli.to_string(),
        });
    }
    if let Some(priority) = candidate.priority {
        inputs.push(SwarmNextActionScoreInput {
            name: "priority",
            value: priority.to_string(),
        });
    }
    inputs.sort();
    inputs
}

fn suggested_reservations_for_candidate(
    candidate: &SwarmNextActionCandidate,
    decision: &'static str,
) -> Vec<SwarmNextActionSuggestedReservation> {
    if matches!(
        decision,
        "duplicate_rejected" | "blocked_by_owner" | "reuse_recent_evidence" | "blocked_rollup"
    ) {
        return Vec::new();
    }

    let mut reservations = BTreeMap::<String, &'static str>::new();
    reservations.insert(
        ".beads/issues.jsonl".to_owned(),
        "claim_and_close_tracker_state",
    );

    let title = candidate.title.to_ascii_lowercase();
    if title.contains("swarm next-action") || title.contains("next-action") {
        reservations.insert(
            "src/core/swarm_next_action.rs".to_owned(),
            "next_action_ranking_surface",
        );
        reservations.insert(
            "docs/schemas/ee.swarm_next_action.v1.json".to_owned(),
            "next_action_schema_surface",
        );
    }
    if title.contains("db") || title.contains("sqlmodel") {
        reservations.insert("src/db/**".to_owned(), "storage_schema_surface");
    }
    if title.contains("policy") || title.contains("redaction") || title.contains("trust") {
        reservations.insert("src/policy/**".to_owned(), "policy_surface");
        reservations.insert("src/models/**".to_owned(), "domain_model_surface");
    }
    if title.contains("search") || title.contains("index") || title.contains("embed") {
        reservations.insert("src/search/**".to_owned(), "search_index_surface");
    }
    if title.contains("pack") || title.contains("context") {
        reservations.insert("src/pack/**".to_owned(), "context_pack_surface");
    }

    reservations
        .into_iter()
        .map(
            |(path_pattern, reason)| SwarmNextActionSuggestedReservation {
                path_pattern,
                exclusive: true,
                reason,
            },
        )
        .collect()
}

fn do_not_take_reasons_for_candidate(
    candidate: &SwarmNextActionCandidate,
    decision: &'static str,
    duplicate: bool,
    blocked_by_owner: bool,
    evidence_caveats: &[String],
    reusable_verifier_evidence: &[String],
) -> Vec<String> {
    let mut reasons = BTreeSet::new();
    if duplicate {
        reasons.insert("candidate_already_appears_in_multiple_sources".to_owned());
    }
    if let Some(assignee) = &candidate.assignee {
        reasons.insert(format!("candidate_assigned_to:{assignee}"));
    }
    if blocked_by_owner {
        reasons.insert("active_owner_or_compile_health_blocker_present".to_owned());
    }
    if !candidate.blocked_by.is_empty() {
        reasons.insert(format!("blocked_by:{}", candidate.blocked_by.join(",")));
    }
    if candidate.blocked_by_compile_health {
        reasons.insert("dirty_compile_health_blocks_rch".to_owned());
    }
    if candidate_is_rollup(candidate) {
        if let Some(issue_type) = &candidate.issue_type {
            reasons.insert(format!("candidate_issue_type:{issue_type}"));
        }
        reasons.insert("rollup_candidate_not_claimable".to_owned());
        reasons.insert("claim_concrete_child_bead_instead".to_owned());
    }
    if matches!(
        decision,
        "duplicate_rejected" | "blocked_by_owner" | "blocked_rollup"
    ) {
        reasons.extend(evidence_caveats.iter().cloned());
    }
    if decision == "reuse_recent_evidence" {
        reasons.insert("recent_verifier_evidence_available".to_owned());
        reasons.extend(reusable_verifier_evidence.iter().cloned());
    }
    reasons.into_iter().collect()
}

fn recommendation_proof_obligations(
    candidate: &SwarmNextActionCandidate,
    decision: &'static str,
) -> Vec<String> {
    let mut obligations = BTreeSet::from([
        "record_overlap_decision_in_closeout".to_owned(),
        "reserve_files_before_editing".to_owned(),
        "use_rch_for_cargo_verification".to_owned(),
    ]);
    if candidate.source == "bv_top_pick" {
        obligations.insert("preserve_bv_reasoning_in_beads_comment".to_owned());
    }
    if candidate.blocked_by_compile_health || decision == "blocked_by_owner" {
        obligations.insert("coordinate_compile_health_blocker_before_rch".to_owned());
    }
    if decision == "new_bead_recommended" {
        obligations.insert("search_existing_beads_before_creation".to_owned());
    }
    if decision == "reuse_recent_evidence" {
        obligations.insert("record_reused_verification_hash_in_closeout".to_owned());
        obligations.insert("avoid_duplicate_rch_until_source_changes".to_owned());
    }
    if decision == "blocked_rollup" {
        obligations.insert("inspect_claimable_child_bead_before_any_claim".to_owned());
        obligations.insert("do_not_claim_epic_or_theme_rollup".to_owned());
    }
    obligations.into_iter().collect()
}

fn reusable_verifier_evidence_for_snapshot(snapshot: &SwarmNextActionSnapshot) -> Vec<String> {
    let mut evidence = BTreeSet::new();
    for blocker in &snapshot.compile_health.blockers {
        let Some(first_error) = &blocker.recent_first_error else {
            continue;
        };
        evidence.insert(format!("recent_verifier_path:{}", blocker.path));
        evidence.insert(format!("recent_verifier_reason:{}", blocker.reason));
        if let Some(status) = &first_error.status {
            evidence.insert(format!("recent_verifier_status:{status}"));
        }
        if let Some(hash) = &first_error.command_hash {
            evidence.insert(format!("recent_verifier_command_hash:{hash}"));
        }
        for kind in &blocker.affected_command_kinds {
            evidence.insert(format!("recent_verifier_command_kind:{kind}"));
        }
        if let Some(command) = &first_error.command
            && let Some(target) = cargo_test_target_from_command(command)
        {
            evidence.insert(format!("recent_verifier_command_target:{target}"));
        }
    }
    evidence.into_iter().collect()
}

fn cargo_test_target_from_command(command: &str) -> Option<String> {
    let words = command.split_whitespace().collect::<Vec<_>>();
    let cargo_index = words.iter().position(|word| *word == "cargo")?;
    if words.get(cargo_index + 1).copied() != Some("test") {
        return None;
    }

    let mut index = cargo_index + 2;
    while index < words.len() {
        match words[index] {
            "--" => break,
            "--lib" => return Some("--lib".to_owned()),
            "--package" | "--test" | "-p" => {
                let flag = words[index];
                let value = words.get(index + 1)?;
                return Some(format!("{flag}:{value}"));
            }
            token if !token.starts_with('-') => return Some(token.to_owned()),
            _ => index += 1,
        }
    }
    Some("--workspace".to_owned())
}

fn recommendation_evidence_caveats(snapshot: &SwarmNextActionSnapshot) -> Vec<String> {
    let mut caveats = BTreeSet::new();
    if snapshot.checkout.dirty_path_count > 0 {
        caveats.insert(format!(
            "dirty_checkout_paths:{}",
            snapshot.checkout.dirty_path_count
        ));
    }
    match snapshot.compile_health.safe_to_launch_rch {
        Some(false) => {
            caveats.insert("compile_health_blocks_rch".to_owned());
        }
        None => {
            caveats.insert("compile_health_uncertain".to_owned());
        }
        Some(true) => {}
    }
    if snapshot.verification.remote_only_required
        && snapshot.verification.remote_only_safe == Some(false)
    {
        caveats.insert("remote_only_rch_not_safe".to_owned());
    }
    if snapshot.verification.head_of_line_blocked() == Some(true) {
        caveats.insert("rch_head_of_line_blocked".to_owned());
    }
    if snapshot
        .verification
        .suspected_orphaned_queued_verifier_count()
        .is_some_and(|count| count > 0)
    {
        caveats.insert("rch_orphaned_queue_possible".to_owned());
    }
    for degradation in &snapshot.degraded {
        caveats.insert(format!(
            "degraded:{}:{}",
            degradation.source, degradation.code
        ));
    }
    caveats.into_iter().collect()
}

fn recommendation_confidence(
    candidate: &SwarmNextActionCandidate,
    decision: &'static str,
    evidence_caveats: &[String],
) -> &'static str {
    if matches!(
        decision,
        "duplicate_rejected" | "blocked_by_owner" | "blocked_rollup" | "no_action_recommended"
    ) || candidate.blocked_by_compile_health
    {
        return "low";
    }
    if !evidence_caveats.is_empty() || candidate.score_milli.is_some_and(|score| score < 500) {
        return "medium";
    }
    if candidate.score_milli.is_some_and(|score| score >= 700)
        || candidate.priority.is_some_and(|priority| priority <= 2)
    {
        return "high";
    }
    "medium"
}

fn compile_health_summary(
    brief: &SwarmBriefReport,
    verifier_evidence: &[SwarmNextActionRecentFirstError],
) -> SwarmNextActionCompileHealthSummary {
    let mut evidence_by_path: BTreeMap<String, Vec<SwarmNextActionRecentFirstError>> =
        BTreeMap::new();
    for evidence in verifier_evidence {
        evidence_by_path
            .entry(evidence.file.clone())
            .or_default()
            .push(evidence.clone());
    }
    let mut blockers = brief
        .dirty_files
        .iter()
        .filter(|file| is_compile_critical_path(&file.path))
        .map(|file| {
            compile_health_blocker_for_path(
                &file.path,
                &brief.file_reservations,
                evidence_by_path.get(&file.path).map(Vec::as_slice),
            )
        })
        .collect::<Vec<_>>();
    blockers.sort();
    blockers.dedup();

    let safe_to_launch_rch = if blockers.iter().any(|blocker| blocker.severity == "high") {
        Some(false)
    } else if blockers.is_empty() {
        Some(true)
    } else {
        None
    };

    let recommended_alternative_work = match safe_to_launch_rch {
        Some(true) => vec!["launch_rch_when_other_verification_inputs_are_ready".to_owned()],
        Some(false) => vec![
            "message_compile_blocker_owner_before_rch".to_owned(),
            "prefer_static_or_non_cargo_work".to_owned(),
        ],
        None => vec![
            "prefer_static_or_non_cargo_work".to_owned(),
            "collect_or_refresh_compile_health_evidence".to_owned(),
        ],
    };

    SwarmNextActionCompileHealthSummary {
        safe_to_launch_rch,
        blocker_count: blockers.len(),
        blockers,
        recommended_alternative_work,
    }
}

fn compile_health_blocker_for_path(
    path: &str,
    reservations: &[SwarmBriefFileReservation],
    verifier_evidence: Option<&[SwarmNextActionRecentFirstError]>,
) -> SwarmNextActionCompileHealthBlocker {
    let owner = reservations
        .iter()
        .filter(|reservation| reservation.exclusive)
        .find(|reservation| path_matches_pattern(path, &reservation.path_pattern));
    let recent_first_error = verifier_evidence.and_then(|items| items.first().cloned());
    let affected_command_kinds = verifier_evidence
        .map(affected_command_kinds)
        .unwrap_or_default();
    match owner {
        Some(reservation) => SwarmNextActionCompileHealthBlocker {
            path: path.to_owned(),
            severity: "high",
            reason: "dirty_compile_critical_path_reserved_by_other_agent",
            owner_agent: Some(reservation.holder.clone()),
            owner_pattern: Some(reservation.path_pattern.clone()),
            recent_first_error,
            affected_command_kinds,
            suggested_next_action: "message_owner_before_rch",
        },
        None => SwarmNextActionCompileHealthBlocker {
            path: path.to_owned(),
            severity: if recent_first_error.is_some() {
                "high"
            } else {
                "medium"
            },
            reason: if recent_first_error.is_some() {
                "recent_rch_first_error_matches_dirty_path"
            } else {
                "dirty_compile_critical_path_without_owner"
            },
            owner_agent: None,
            owner_pattern: None,
            recent_first_error,
            affected_command_kinds,
            suggested_next_action: "prefer_static_or_non_cargo_work_until_compile_health_is_known",
        },
    }
}

fn affected_command_kinds(items: &[SwarmNextActionRecentFirstError]) -> Vec<String> {
    let mut kinds = BTreeSet::new();
    for item in items {
        if let Some(kind) = &item.command_kind {
            kinds.insert(kind.clone());
        } else if let Some(command) = &item.command {
            kinds.insert(command_kind_from_text(command).to_owned());
        }
    }
    kinds.into_iter().collect()
}

fn command_kind_from_text(command: &str) -> &'static str {
    if command.contains("cargo test") {
        "cargo_test"
    } else if command.contains("cargo check") {
        "cargo_check"
    } else if command.contains("cargo clippy") {
        "cargo_clippy"
    } else if command.contains("cargo bench") {
        "cargo_bench"
    } else if command.contains("cargo fmt") {
        "cargo_fmt_check"
    } else {
        "unknown"
    }
}

fn is_compile_critical_path(path: &str) -> bool {
    path == "Cargo.toml"
        || path == "Cargo.lock"
        || path.ends_with(".rs")
        || path.ends_with("/Cargo.toml")
        || path.ends_with("/build.rs")
}

fn path_matches_pattern(path: &str, pattern: &str) -> bool {
    path == pattern || wildcard_matches(pattern.as_bytes(), path.as_bytes())
}

fn wildcard_matches(pattern: &[u8], text: &[u8]) -> bool {
    let (mut pattern_index, mut text_index) = (0, 0);
    let mut star_index = None;
    let mut star_text_index = 0;

    while text_index < text.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == text[text_index] || pattern[pattern_index] == b'?')
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_text_index = text_index;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            star_text_index += 1;
            text_index = star_text_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn coordination_summary(brief: &SwarmBriefReport) -> SwarmNextActionCoordinationSummary {
    let mut holders = brief
        .file_reservations
        .iter()
        .map(|reservation| reservation.holder.clone())
        .collect::<Vec<_>>();
    holders.sort();
    holders.dedup();
    SwarmNextActionCoordinationSummary {
        active_reservation_count: brief.file_reservations.len(),
        reservation_holders: holders,
        unread_inbox_count: brief.inbox.iter().map(|entry| entry.unread_count).sum(),
        ack_required_count: brief
            .inbox
            .iter()
            .map(|entry| entry.ack_required_count)
            .sum(),
    }
}

fn verification_summary(
    brief: &SwarmBriefReport,
    verifier_evidence: &[SwarmNextActionRecentFirstError],
) -> SwarmNextActionVerificationSummary {
    let rch = brief.rch_local_capability.as_ref();
    SwarmNextActionVerificationSummary {
        rch_source_enabled: rch.is_some()
            || brief
                .sources
                .iter()
                .any(|source| source.source == SwarmBriefSourceKind::Rch)
            || !verifier_evidence.is_empty(),
        remote_only_required: rch.is_some_and(|report| report.remote_only_required)
            || verifier_evidence.iter().any(|evidence| {
                evidence.remote_required == Some(true) || evidence.local_fallback_refused
            }),
        remote_only_safe: rch.map(|report| report.remote_only_safe),
        healthy_worker_count: rch.map(|report| report.worker_probe_summary.healthy_count),
        active_remote_build_count: rch
            .and_then(|report| report.queue_health.as_ref())
            .map(|queue| queue.active_count),
        queued_remote_build_count: rch
            .and_then(|report| report.queue_health.as_ref())
            .map(|queue| queue.queued_count),
        slots_available: rch
            .and_then(|report| report.queue_health.as_ref())
            .and_then(|queue| queue.slots_available),
        queue_head_slots_needed: rch
            .and_then(|report| report.queue_health.as_ref())
            .and_then(|queue| queue.queue_head_slots_needed),
        active_build_max_age_seconds: rch
            .and_then(|report| report.queue_health.as_ref())
            .and_then(|queue| queue.active_build_max_age_seconds),
        queue_status: rch
            .and_then(|report| report.queue_health.as_ref())
            .map(|queue| queue.status.clone()),
        verifier_evidence: verifier_evidence.to_vec(),
    }
}

fn environment_summary(brief: &SwarmBriefReport) -> SwarmNextActionEnvironmentSummary {
    SwarmNextActionEnvironmentSummary {
        cargo_target_externalized: env_path_starts_with(
            "CARGO_TARGET_DIR",
            EXTERNAL_AGENT_SPACE_ROOT,
        ),
        tmpdir_externalized: env_path_starts_with("TMPDIR", EXTERNAL_AGENT_SPACE_ROOT),
        external_agent_space_present: Path::new(EXTERNAL_AGENT_SPACE_ROOT).is_dir(),
        disk_pressure_hint_count: brief
            .resource_pressure
            .iter()
            .filter(|hint| hint.level != "info")
            .count(),
    }
}

fn env_path_starts_with(key: &str, expected_root: &str) -> bool {
    env::var_os(key).is_some_and(|value| Path::new(&value).starts_with(expected_root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::swarm_brief::{
        RchCodexHookCapability, RchLocalCapabilityReport, RchQueueHealth, RchWorkerPressureReport,
        RchWorkerProbeSummary, SwarmBriefBead, SwarmBriefBvPick, SwarmBriefBvSummary,
        SwarmBriefCommit, SwarmBriefDegradation, SwarmBriefDirtyFile, SwarmBriefFileReservation,
        SwarmBriefInboxSummary, SwarmBriefSourceKind, SwarmBriefThreadSummary,
    };

    fn unknown_worker_pressure() -> RchWorkerPressureReport {
        RchWorkerPressureReport {
            schema: "ee.rch.worker_pressure.v1",
            status: "pressure_unknown".to_owned(),
            worker_count: 0,
            usable_worker_count: 0,
            blocked_worker_count: 0,
            stale_worker_count: 0,
            unknown_worker_count: 0,
            workers: Vec::new(),
        }
    }

    #[test]
    fn next_action_snapshot_deduplicates_and_orders_candidates() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.ready = vec![
            bead("bd-b", "Second", 2),
            bead("bd-a", "First", 1),
            bead("bd-a", "First duplicate", 1),
        ];
        brief.bv = Some(SwarmBriefBvSummary {
            actionable_count: Some(2),
            blocked_count: Some(0),
            in_progress_count: Some(0),
            track_count: None,
            top_picks: vec![SwarmBriefBvPick {
                id: "bd-b".to_owned(),
                title: "Second".to_owned(),
                score_milli: Some(900),
                action_hint: Some("Work on bd-a first".to_owned()),
                blocked_by: vec!["bd-a".to_owned()],
            }],
        });

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        assert_eq!(snapshot.schema, SWARM_NEXT_ACTION_SCHEMA_V1);
        assert_eq!(snapshot.inputs.ready_bead_count, 3);
        assert_eq!(
            snapshot
                .candidates
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            vec!["bd-a", "bd-b"]
        );
        assert_eq!(snapshot.candidates[1].source, "bv_top_pick");
        assert_eq!(snapshot.candidates[1].score_milli, Some(900));
        assert_eq!(snapshot.candidates[1].blocked_by, vec!["bd-a"]);
        assert!(!snapshot.candidates[1].blocked_by_compile_health);
        assert_eq!(snapshot.candidates[1].action_hint, "Work on bd-a first");
    }

    #[test]
    fn next_action_bv_pick_inherits_beads_issue_type_for_rollup_downgrade() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut epic = bead(
            "bd-epic",
            "[idea-wizard] Epic: parent wrapper with no implementation leaf",
            1,
        );
        epic.issue_type = Some("epic".to_owned());
        brief.beads.ready = vec![epic];
        brief.bv = Some(SwarmBriefBvSummary {
            actionable_count: Some(1),
            blocked_count: Some(0),
            in_progress_count: Some(0),
            track_count: None,
            top_picks: vec![SwarmBriefBvPick {
                id: "bd-epic".to_owned(),
                title: "[idea-wizard] Epic: parent wrapper with no implementation leaf".to_owned(),
                score_milli: Some(925),
                action_hint: Some("Inspect concrete children before claiming.".to_owned()),
                blocked_by: Vec::new(),
            }],
        });

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);
        let cards = snapshot.recommendation_cards();

        assert_eq!(snapshot.candidates.len(), 1);
        assert_eq!(snapshot.candidates[0].source, "bv_top_pick");
        assert_eq!(snapshot.candidates[0].issue_type.as_deref(), Some("epic"));
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].decision, "blocked_rollup");
        assert!(cards[0].suggested_reservations.is_empty());
        assert!(
            cards[0]
                .do_not_take_because
                .contains(&"candidate_issue_type:epic".to_owned())
        );
        assert!(
            cards[0]
                .do_not_take_because
                .contains(&"claim_concrete_child_bead_instead".to_owned())
        );

        let json = serde_json::to_value(&snapshot).expect("snapshot serializes");
        let candidate_json = json
            .pointer("/candidates/0")
            .and_then(Value::as_object)
            .expect("candidate JSON object");
        assert!(
            !candidate_json.contains_key("issueType"),
            "issue_type remains internal routing metadata"
        );
    }

    #[test]
    fn next_action_snapshot_summarizes_coordination_and_rch_without_bodies() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.file_reservations = vec![
            SwarmBriefFileReservation {
                path_pattern: "src/a.rs".to_owned(),
                holder: "BlueLake".to_owned(),
                exclusive: true,
                expires_at: None,
            },
            SwarmBriefFileReservation {
                path_pattern: "src/b.rs".to_owned(),
                holder: "BlueLake".to_owned(),
                exclusive: true,
                expires_at: None,
            },
        ];
        brief.inbox = vec![SwarmBriefInboxSummary {
            mailbox: "FuchsiaCliff".to_owned(),
            unread_count: 3,
            ack_required_count: 1,
        }];
        brief.rch_local_capability = Some(RchLocalCapabilityReport {
            schema: "ee.rch.local_capability.v1",
            cli_version: Some("0.1.3".to_owned()),
            direct_exec_available: true,
            codex_hook: RchCodexHookCapability {
                installed: true,
                status: "ready".to_owned(),
            },
            daemon_status_socket: None,
            status_socket_consistent: None,
            dry_run_would_offload: Some(true),
            worker_probe_summary: RchWorkerProbeSummary {
                healthy_count: 1,
                failed_count: 0,
                status: "healthy".to_owned(),
            },
            queue_health: Some(RchQueueHealth {
                queued_count: 2,
                active_count: 4,
                slots_available: Some(0),
                queue_head_slots_needed: Some(4),
                active_build_max_age_seconds: Some(3_600),
                status: "saturated".to_owned(),
            }),
            worker_pressure: unknown_worker_pressure(),
            remote_only_required: true,
            remote_only_safe: false,
            degraded: Vec::new(),
            recovery: Vec::new(),
        });

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        assert_eq!(snapshot.coordination.active_reservation_count, 2);
        assert_eq!(snapshot.coordination.reservation_holders, vec!["BlueLake"]);
        assert_eq!(snapshot.coordination.unread_inbox_count, 3);
        assert_eq!(snapshot.coordination.ack_required_count, 1);
        assert_eq!(snapshot.verification.healthy_worker_count, Some(1));
        assert_eq!(snapshot.verification.active_remote_build_count, Some(4));
        assert_eq!(snapshot.verification.queued_remote_build_count, Some(2));
        assert_eq!(snapshot.verification.slots_available, Some(0));
        assert_eq!(snapshot.verification.queue_head_slots_needed, Some(4));
        assert_eq!(
            snapshot.verification.active_build_max_age_seconds,
            Some(3_600)
        );
        assert_eq!(snapshot.verification.head_of_line_blocked(), Some(false));
        assert_eq!(
            snapshot.verification.queue_status.as_deref(),
            Some("saturated")
        );
    }

    #[test]
    fn next_action_verification_marks_head_of_line_convoy_with_evidence() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.ready = vec![bead("bd-static", "Source-only work", 2)];
        brief.rch_local_capability = Some(RchLocalCapabilityReport {
            schema: "ee.rch.local_capability.v1",
            cli_version: Some("1.0.24".to_owned()),
            direct_exec_available: true,
            codex_hook: RchCodexHookCapability {
                installed: true,
                status: "ready".to_owned(),
            },
            daemon_status_socket: None,
            status_socket_consistent: None,
            dry_run_would_offload: Some(true),
            worker_probe_summary: RchWorkerProbeSummary {
                healthy_count: 1,
                failed_count: 0,
                status: "healthy".to_owned(),
            },
            queue_health: Some(RchQueueHealth {
                queued_count: 1,
                active_count: 1,
                slots_available: Some(2),
                queue_head_slots_needed: Some(4),
                active_build_max_age_seconds: Some(79_200),
                status: "capacity_blocked".to_owned(),
            }),
            worker_pressure: unknown_worker_pressure(),
            remote_only_required: true,
            remote_only_safe: false,
            degraded: Vec::new(),
            recovery: Vec::new(),
        });

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        assert_eq!(snapshot.verification.head_of_line_blocked(), Some(true));
        assert!(
            recommendation_evidence_caveats(&snapshot)
                .contains(&"rch_head_of_line_blocked".to_owned())
        );
        let json = serde_json::to_value(&snapshot.verification).expect("verification serializes");
        assert_eq!(json["headOfLineBlocked"], true);
        assert_eq!(
            json["queueRecommendation"],
            "prefer_static_work_until_queue_head_fits"
        );
        assert_eq!(json["activeBuildMaxAgeSeconds"], 79_200);
        assert_eq!(
            json["queueEvidence"],
            serde_json::json!([
                "active_build_max_age_seconds:79200",
                "active_remote_build_count:1",
                "head_of_line_blocked:true",
                "queue_head_slots_needed:4",
                "queue_status:capacity_blocked",
                "queued_remote_build_count:1",
                "slots_available:2"
            ])
        );
        assert_eq!(
            json["admissionCertificate"],
            serde_json::json!({
                "schema": "ee.swarm_next_action.rch_admission_certificate.v1",
                "action": "static_work",
                "ruleId": "rch_admission.head_of_line_convoy",
                "confidence": "high",
                "serviceClass": "cargo_verifier",
                "serviceTimeClass": "long",
                "serviceTimeIntervalMs": {
                    "lower": 300000,
                    "upper": 1800000
                },
                "queueRiskClass": "high",
                "predictorCoverage": "missing",
                "predictorMode": "fallback",
                "conservativeReason": "missing_history",
                "evidence": [
                    "active_build_max_age_seconds:79200",
                    "active_remote_build_count:1",
                    "head_of_line_blocked:true",
                    "queue_head_slots_needed:4",
                    "queue_status:capacity_blocked",
                    "queued_remote_build_count:1",
                    "slots_available:2"
                ],
                "assumptions": [
                    "remote_cargo_is_required_for_build_or_test_work",
                    "certificate_is_advisory_and_never_mutates_rch_state",
                    "queue_head_needs_more_slots_than_currently_available"
                ],
                "proofObligations": [
                    "do_not_launch_duplicate_verifier_without_capacity_evidence",
                    "record_admission_decision_in_closeout",
                    "prefer_static_work_until_queue_head_fits"
                ],
                "safetyInvariant": "monotonic_queue_pressure_never_increases_aggression"
            })
        );
    }

    #[test]
    fn next_action_verification_flags_suspected_orphaned_queued_verifier() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.ready = vec![bead("bd-static", "Static proof while queue is stale", 2)];
        brief.rch_local_capability = Some(RchLocalCapabilityReport {
            schema: "ee.rch.local_capability.v1",
            cli_version: Some("1.0.24".to_owned()),
            direct_exec_available: true,
            codex_hook: RchCodexHookCapability {
                installed: true,
                status: "ready".to_owned(),
            },
            daemon_status_socket: None,
            status_socket_consistent: None,
            dry_run_would_offload: Some(true),
            worker_probe_summary: RchWorkerProbeSummary {
                healthy_count: 1,
                failed_count: 0,
                status: "healthy".to_owned(),
            },
            queue_health: Some(RchQueueHealth {
                queued_count: 1,
                active_count: 0,
                slots_available: Some(4),
                queue_head_slots_needed: Some(4),
                active_build_max_age_seconds: None,
                status: "start_stalled".to_owned(),
            }),
            worker_pressure: unknown_worker_pressure(),
            remote_only_required: true,
            remote_only_safe: false,
            degraded: Vec::new(),
            recovery: Vec::new(),
        });

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        assert_eq!(
            snapshot
                .verification
                .suspected_orphaned_queued_verifier_count(),
            Some(1)
        );
        assert!(
            recommendation_evidence_caveats(&snapshot)
                .contains(&"rch_orphaned_queue_possible".to_owned())
        );
        let json = serde_json::to_value(&snapshot.verification).expect("verification serializes");
        assert_eq!(
            json["queueRecommendation"],
            "avoid_duplicate_verifier_until_orphaned_queue_is_explained"
        );
        assert_eq!(
            json["queueEvidence"],
            serde_json::json!([
                "active_remote_build_count:0",
                "head_of_line_blocked:false",
                "orphaned_queue_cancelability:unknown",
                "orphaned_queue_cleanup:coordination_first",
                "queue_head_slots_needed:4",
                "queue_status:start_stalled",
                "queued_remote_build_count:1",
                "slots_available:4",
                "suspected_orphaned_queued_verifier_count:1"
            ])
        );
        assert_eq!(json["admissionCertificate"]["action"], "coordinate");
        assert_eq!(
            json["admissionCertificate"]["ruleId"],
            "rch_admission.orphaned_queued_verifier"
        );
        assert!(
            json["admissionCertificate"]["proofObligations"]
                .as_array()
                .expect("proof obligations array")
                .iter()
                .any(|value| value == "coordinate_before_queue_cleanup_or_retry")
        );
        assert_eq!(json["admissionCertificate"]["queueRiskClass"], "blocked");
        assert_eq!(
            json["admissionCertificate"]["serviceTimeClass"],
            "long_tail"
        );
    }

    #[test]
    fn next_action_admission_certificate_is_conservative_under_queue_pressure() {
        let ready = verification_for_queue_posture(
            Some(true),
            Some(0),
            Some(0),
            Some(2),
            None,
            None,
            Some("ready"),
        );
        let saturated = verification_for_queue_posture(
            Some(false),
            Some(1),
            Some(0),
            Some(0),
            None,
            None,
            Some("saturated"),
        );
        let convoy = verification_for_queue_posture(
            Some(false),
            Some(1),
            Some(1),
            Some(2),
            Some(4),
            Some(3_600),
            Some("capacity_blocked"),
        );
        let missing = verification_for_queue_posture(None, None, None, None, None, None, None);

        let decisions = [
            ready.admission_certificate().action,
            saturated.admission_certificate().action,
            convoy.admission_certificate().action,
            missing.admission_certificate().action,
        ];
        assert_eq!(decisions, ["queue", "wait", "static_work", "coordinate"]);
        assert!(
            decisions
                .windows(2)
                .all(|window| admission_aggression_rank(window[0])
                    >= admission_aggression_rank(window[1])),
            "adding queue pressure must not make the admission decision more aggressive: {decisions:?}"
        );
        assert_eq!(
            convoy.admission_certificate().safety_invariant,
            "monotonic_queue_pressure_never_increases_aggression"
        );
    }

    #[test]
    fn next_action_service_time_estimator_uses_calibrated_history_when_healthy() {
        let verification = verification_for_queue_posture(
            Some(true),
            Some(0),
            Some(0),
            Some(3),
            None,
            None,
            Some("ready"),
        );
        let records = service_records(&[90_000, 100_000, 120_000, 140_000, 160_000, 180_000]);

        let estimate = verification.service_time_estimate_with_history(&records);

        assert_eq!(estimate.predictor_mode, "calibrated");
        assert_eq!(estimate.predictor_coverage, "healthy");
        assert_eq!(estimate.queue_risk_class, "low");
        assert_eq!(estimate.conservative_reason, None);
        assert_eq!(estimate.service_time_class, "medium");
        assert!(estimate.service_time_interval_ms.lower >= 90_000);
        assert!(estimate.service_time_interval_ms.upper <= 240_000);
    }

    #[test]
    fn next_action_service_time_estimator_falls_back_for_sparse_missing_and_stale_history() {
        let verification = verification_for_queue_posture(
            Some(true),
            Some(0),
            Some(1),
            Some(2),
            None,
            None,
            Some("ready"),
        );
        let sparse = verification.service_time_estimate_with_history(&service_records(&[100_000]));
        let missing = verification.service_time_estimate_with_history(&[]);
        let mut stale_records = service_records(&[100_000, 110_000, 120_000, 130_000, 140_000]);
        for record in &mut stale_records {
            record.observed_age_seconds = 8 * 24 * 60 * 60;
        }
        let stale = verification.service_time_estimate_with_history(&stale_records);

        assert_eq!(sparse.conservative_reason, Some("sparse_history"));
        assert_eq!(missing.conservative_reason, Some("missing_history"));
        assert_eq!(stale.conservative_reason, Some("stale_history"));
        assert_eq!(sparse.predictor_mode, "fallback");
        assert_eq!(missing.predictor_mode, "fallback");
        assert_eq!(stale.predictor_mode, "fallback");
        assert_eq!(sparse.queue_risk_class, "medium");
    }

    #[test]
    fn next_action_service_time_estimator_falls_back_for_heavy_tail_and_miscalibration() {
        let verification = verification_for_queue_posture(
            Some(true),
            Some(0),
            Some(0),
            Some(2),
            None,
            None,
            Some("ready"),
        );
        let heavy_tail = verification.service_time_estimate_with_history(&service_records(&[
            40_000, 41_000, 42_000, 43_000, 44_000, 900_000,
        ]));
        let mut miscalibrated = service_records(&[90_000, 100_000, 110_000, 120_000, 130_000]);
        miscalibrated[2].failure_class = Some("coverage_miss".to_owned());
        let miscalibrated = verification.service_time_estimate_with_history(&miscalibrated);

        assert_eq!(heavy_tail.conservative_reason, Some("heavy_tailed_history"));
        assert_eq!(
            miscalibrated.conservative_reason,
            Some("miscalibrated_predictor")
        );
        assert_eq!(heavy_tail.predictor_coverage, "heavy_tailed");
        assert_eq!(miscalibrated.predictor_coverage, "miscalibrated");
    }

    #[test]
    fn next_action_service_time_estimator_duplicate_queued_verifier_is_conservative() {
        let verification = verification_for_queue_posture(
            Some(true),
            Some(1),
            Some(1),
            Some(1),
            None,
            None,
            Some("busy"),
        );
        let mut records = service_records(&[80_000, 90_000, 100_000, 110_000, 120_000]);
        records[0].duplicate_bead_attribution = true;

        let estimate = verification.service_time_estimate_with_history(&records);

        assert_eq!(
            estimate.conservative_reason,
            Some("duplicate_queued_verifier")
        );
        assert_eq!(estimate.predictor_mode, "fallback");
        assert_eq!(estimate.queue_risk_class, "medium");
    }

    #[test]
    fn next_action_snapshot_sorts_and_deduplicates_degradations() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.degraded = vec![
            degradation(
                SwarmBriefSourceKind::Bv,
                "bv_unavailable",
                "BV robot triage was unavailable.",
                Some("Run bv --robot-triage after repairing bv.".to_owned()),
            ),
            degradation(
                SwarmBriefSourceKind::AgentMail,
                "agent_mail_unavailable",
                "Agent Mail state was unavailable.",
                None,
            ),
            degradation(
                SwarmBriefSourceKind::Bv,
                "bv_unavailable",
                "BV robot triage was unavailable.",
                Some("Run bv --robot-triage after repairing bv.".to_owned()),
            ),
        ];

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        assert_eq!(
            snapshot
                .degraded
                .iter()
                .map(|degradation| (
                    degradation.code.as_str(),
                    degradation.source.as_str(),
                    degradation.severity,
                    degradation.repair.as_deref(),
                ))
                .collect::<Vec<_>>(),
            vec![
                ("agent_mail_unavailable", "agent_mail", "warning", None),
                (
                    "bv_unavailable",
                    "bv",
                    "warning",
                    Some("Run bv --robot-triage after repairing bv."),
                ),
            ]
        );
    }

    #[test]
    fn next_action_compile_health_blocks_candidates_for_reserved_dirty_rust_paths() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.ready = vec![bead("bd-rch", "Needs RCH proof", 1)];
        brief.dirty_files = vec![
            SwarmBriefDirtyFile {
                path: "src/db/mod.rs".to_owned(),
                status: "M".to_owned(),
            },
            SwarmBriefDirtyFile {
                path: "docs/rch_verification.md".to_owned(),
                status: "M".to_owned(),
            },
        ];
        brief.file_reservations = vec![SwarmBriefFileReservation {
            path_pattern: "src/db/*.rs".to_owned(),
            holder: "CloudyHawk".to_owned(),
            exclusive: true,
            expires_at: Some("2026-05-18T10:00:00Z".to_owned()),
        }];

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        assert_eq!(snapshot.compile_health.safe_to_launch_rch, Some(false));
        assert_eq!(snapshot.compile_health.blocker_count, 1);
        assert_eq!(
            snapshot.compile_health.blockers[0],
            SwarmNextActionCompileHealthBlocker {
                path: "src/db/mod.rs".to_owned(),
                severity: "high",
                reason: "dirty_compile_critical_path_reserved_by_other_agent",
                owner_agent: Some("CloudyHawk".to_owned()),
                owner_pattern: Some("src/db/*.rs".to_owned()),
                recent_first_error: None,
                affected_command_kinds: Vec::new(),
                suggested_next_action: "message_owner_before_rch",
            }
        );
        assert!(snapshot.candidates[0].blocked_by_compile_health);
        assert!(
            snapshot
                .compile_health
                .recommended_alternative_work
                .contains(&"message_compile_blocker_owner_before_rch".to_owned())
        );
    }

    #[test]
    fn next_action_compile_health_unknown_for_unowned_dirty_rust_paths() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.ready = vec![bead("bd-static", "Static-only slice", 2)];
        brief.dirty_files = vec![SwarmBriefDirtyFile {
            path: "src/core/status.rs".to_owned(),
            status: "M".to_owned(),
        }];

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        assert_eq!(snapshot.compile_health.safe_to_launch_rch, None);
        assert_eq!(snapshot.compile_health.blocker_count, 1);
        assert_eq!(
            snapshot.compile_health.blockers[0].reason,
            "dirty_compile_critical_path_without_owner"
        );
        assert!(!snapshot.candidates[0].blocked_by_compile_health);
    }

    #[test]
    fn next_action_compile_health_uses_recent_verifier_first_error_for_dirty_path() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.ready = vec![bead("bd-ppr", "Needs focused PPR proof", 1)];
        brief.dirty_files = vec![SwarmBriefDirtyFile {
            path: "src/db/mod.rs".to_owned(),
            status: "M".to_owned(),
        }];
        let evidence = vec![SwarmNextActionRecentFirstError {
            file: "src/db/mod.rs".to_owned(),
            line: Some(431),
            command_kind: Some("cargo_test".to_owned()),
            command: Some("cargo test --lib ppr_proof -- --nocapture".to_owned()),
            command_hash: Some("abc123".to_owned()),
            status: Some("remote_failure".to_owned()),
            degraded_codes: vec!["rch_verify_remote_command_failed".to_owned()],
            source_state_hash: None,
            created_at: None,
            error_codes: Vec::new(),
            remote_required: Some(true),
            local_fallback_refused: false,
            retry_after: None,
            known_blocker: None,
        }];

        let snapshot =
            SwarmNextActionSnapshot::from_swarm_brief_with_verifier_evidence(&brief, &evidence);

        assert_eq!(snapshot.compile_health.safe_to_launch_rch, Some(false));
        assert!(snapshot.candidates[0].blocked_by_compile_health);
        let blocker = &snapshot.compile_health.blockers[0];
        assert_eq!(blocker.reason, "recent_rch_first_error_matches_dirty_path");
        assert_eq!(blocker.affected_command_kinds, vec!["cargo_test"]);
        assert_eq!(
            blocker
                .recent_first_error
                .as_ref()
                .and_then(|error| error.line),
            Some(431)
        );
    }

    #[test]
    fn stale_work_proposals_leave_active_assignee_alone_when_reservation_matches() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut bead = bead("bd-active", "Active in-progress work", 2);
        bead.status = "in_progress".to_owned();
        bead.source_bucket = "in_progress".to_owned();
        bead.assignee = Some("BlueLake".to_owned());
        brief.beads.in_progress = vec![bead];
        brief.file_reservations = vec![SwarmBriefFileReservation {
            path_pattern: "src/db/*.rs".to_owned(),
            holder: "BlueLake".to_owned(),
            exclusive: true,
            expires_at: Some("2026-05-21T16:00:00Z".to_owned()),
        }];

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        assert_eq!(snapshot.stale_work_proposals.len(), 1);
        let proposal = &snapshot.stale_work_proposals[0];
        assert_eq!(proposal.decision, "leaveAloneActive");
        assert_eq!(proposal.confidence, "high");
        assert!(
            proposal
                .evidence
                .iter()
                .any(|entry| entry.starts_with("active_reservation_holder:BlueLake:"))
        );
        assert_eq!(
            proposal.suggested_commands,
            vec!["br show bd-active --json"]
        );
    }

    #[test]
    fn stale_work_proposals_reopen_stale_assignee_with_multiple_missing_signals() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut bead = bead("bd-stale", "Stale in-progress work", 2);
        bead.status = "in_progress".to_owned();
        bead.source_bucket = "in_progress".to_owned();
        bead.assignee = Some("QuietHill".to_owned());
        brief.beads.in_progress = vec![bead];

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        let proposal = &snapshot.stale_work_proposals[0];
        assert_eq!(proposal.decision, "reopenSuggested");
        assert_eq!(proposal.confidence, "medium");
        assert!(
            proposal
                .evidence
                .contains(&"no_matching_active_reservation".to_owned())
        );
        assert!(
            proposal
                .evidence
                .contains(&"no_recent_commit_mentions_bead".to_owned())
        );
        assert!(
            proposal
                .suggested_commands
                .contains(&"br update bd-stale --status open --json".to_owned())
        );
    }

    #[test]
    fn stale_work_proposals_treat_missing_agent_mail_as_caveat_not_stale_evidence() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut bead = bead("bd-mail", "Mail degraded in-progress work", 2);
        bead.status = "in_progress".to_owned();
        bead.source_bucket = "in_progress".to_owned();
        bead.assignee = Some("QuietHill".to_owned());
        brief.beads.in_progress = vec![bead];
        brief.degraded = vec![degradation(
            SwarmBriefSourceKind::AgentMail,
            "agent_mail_unavailable",
            "Agent Mail state was unavailable.",
            None,
        )];

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        let proposal = &snapshot.stale_work_proposals[0];
        assert_eq!(proposal.decision, "contactSuggested");
        assert_eq!(proposal.confidence, "low");
        assert!(
            proposal
                .caveats
                .contains(&"agent_mail_unavailable_not_stale_evidence".to_owned())
        );
    }

    #[test]
    fn stale_work_proposals_keep_recent_commit_and_thread_active() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut bead = bead("bd-recent", "Recent in-progress work", 2);
        bead.status = "in_progress".to_owned();
        bead.source_bucket = "in_progress".to_owned();
        bead.assignee = Some("CoralStone".to_owned());
        brief.beads.in_progress = vec![bead];
        brief.recent_commits = vec![SwarmBriefCommit {
            hash: "abc123".to_owned(),
            authored_at_epoch_seconds: Some(1_768_000_000),
            subject: "fix: continue bd-recent".to_owned(),
        }];
        brief.threads = vec![SwarmBriefThreadSummary {
            thread_id: "bd-recent".to_owned(),
            subject: Some("[bd-recent] progress".to_owned()),
            message_count: Some(3),
            last_activity_at: Some("2026-05-21T14:00:00Z".to_owned()),
        }];

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        let proposal = &snapshot.stale_work_proposals[0];
        assert_eq!(proposal.decision, "leaveAloneActive");
        assert!(
            proposal
                .evidence
                .contains(&"recent_commit_mentions_bead:abc123".to_owned())
        );
        assert!(
            proposal
                .evidence
                .contains(&"mail_thread_mentions_bead:bd-recent".to_owned())
        );
    }

    #[test]
    fn stale_work_proposals_contact_for_blocked_parent_before_reopen() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut bead = bead("bd-blocked", "Blocked in-progress work", 2);
        bead.status = "in_progress".to_owned();
        bead.source_bucket = "in_progress".to_owned();
        bead.assignee = Some("QuietHill".to_owned());
        brief.beads.in_progress = vec![bead];
        brief.bv = Some(SwarmBriefBvSummary {
            actionable_count: Some(0),
            blocked_count: Some(1),
            in_progress_count: Some(1),
            track_count: None,
            top_picks: vec![SwarmBriefBvPick {
                id: "bd-blocked".to_owned(),
                title: "Blocked in-progress work".to_owned(),
                score_milli: Some(650),
                action_hint: Some("Work on bd-parent first".to_owned()),
                blocked_by: vec!["bd-parent".to_owned()],
            }],
        });

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        let proposal = &snapshot.stale_work_proposals[0];
        assert_eq!(proposal.decision, "contactSuggested");
        assert!(
            proposal
                .caveats
                .contains(&"blocked_dependencies_require_parent_check_before_reopen".to_owned())
        );
        assert!(
            proposal
                .evidence
                .contains(&"blocked_by:bd-parent".to_owned())
        );
    }

    #[test]
    fn verifier_evidence_json_parser_extracts_failure_first_error_only() {
        let evidence = verifier_evidence_from_json(&serde_json::json!({
            "runs": [
                {
                    "schema": "ee.rch.verify.v1",
                    "status": "remote_pass",
                    "first_error_file": "src/ignored.rs",
                    "first_error_line": 1
                },
                {
                    "schema": "ee.rch.verify.v1",
                    "status": "remote_failure",
                    "command_text": "cargo test --lib ppr_proof -- --nocapture",
                    "command_hash": "abc123",
                    "first_error_file": "/data/projects/eidetic_engine_cli/src/db/mod.rs",
                    "first_error_line": 431,
                    "degraded_codes": ["rch_verify_remote_command_failed"]
                }
            ]
        }));

        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].file, "src/db/mod.rs");
        assert_eq!(evidence[0].line, Some(431));
        assert_eq!(evidence[0].command_kind, None);
        assert_eq!(
            evidence[0].command.as_deref(),
            Some("cargo test --lib ppr_proof -- --nocapture")
        );
    }

    #[test]
    fn verifier_evidence_json_parser_drops_exact_key_blocker_after_later_success() {
        let evidence = verifier_evidence_from_json(&serde_json::json!({
            "runs": [
                {
                    "schema": "ee.rch.verify.v1",
                    "status": "blocked",
                    "command_text": "cargo test --lib verify_ledger -- --nocapture",
                    "command_hash": "cmd123",
                    "sourceStateHash": "src456",
                    "createdAt": "2026-05-23T05:00:00Z",
                    "remoteRequired": true,
                    "blockerFingerprint": "sha256:blocked",
                    "remediationBead": "bd-17c65.10.17.1.2",
                    "retryAfter": "2026-05-23T07:00:00Z",
                    "degradedCodes": [
                        "rch_verify_topology_blocked",
                        "rch_verify_local_fallback_refused"
                    ]
                },
                {
                    "schema": "ee.rch.verify.v1",
                    "status": "passed",
                    "commandHash": "cmd123",
                    "sourceStateHash": "src456",
                    "createdAt": "2026-05-23T05:30:00Z"
                },
                {
                    "schema": "ee.rch.verify.v1",
                    "status": "blocked",
                    "commandHash": "cmd123",
                    "sourceStateHash": "different-source",
                    "createdAt": "2026-05-23T05:00:00Z",
                    "blockerFingerprint": "sha256:still-blocked",
                    "degradedCodes": ["rch_verify_topology_blocked"]
                }
            ]
        }));

        assert_eq!(evidence.len(), 1);
        assert_eq!(
            evidence[0].source_state_hash.as_deref(),
            Some("different-source")
        );
        let known_blocker = evidence[0].known_blocker.as_ref().expect("known blocker");
        assert_eq!(known_blocker.fingerprint, "sha256:still-blocked");
    }

    #[test]
    fn verifier_evidence_json_parser_extracts_rch_e327_without_first_error() {
        let evidence = verifier_evidence_from_json(&serde_json::json!({
            "schema": "ee.rch.verify.v1",
            "status": "rch_environment_failure",
            "command_text": "cargo test --test rch_verify_contract",
            "command_kind": "cargo_test",
            "command_hash": "cd825533cce8c288",
            "remote_required": true,
            "error_codes": ["RCH-E327"],
            "degraded_codes": [
                "rch_verify_remote_command_failed",
                "rch_verify_topology_blocked",
                "rch_verify_local_fallback_refused",
                "rch_verify_remote_marker_missing"
            ],
            "known_blocker": {
                "blocker_kind": "path_dependency_topology",
                "blocker_fingerprint": "sha256:topology-refusal",
                "remediation_bead": "bd-17c65.10.17.1.2",
                "retry_after": "2026-05-23T07:00:00Z",
                "message": "Path dependency topology policy failed."
            }
        }));

        assert_eq!(evidence.len(), 1);
        let item = &evidence[0];
        assert_eq!(item.file, "");
        assert_eq!(item.error_codes, vec!["RCH-E327"]);
        assert_eq!(item.remote_required, Some(true));
        assert!(item.local_fallback_refused);
        assert!(
            item.degraded_codes
                .contains(&"rch_verify_topology_blocked".to_owned())
        );
        let known_blocker = item.known_blocker.as_ref().expect("known blocker parsed");
        assert_eq!(known_blocker.code, "path_dependency_topology");
        assert_eq!(known_blocker.fingerprint, "sha256:topology-refusal");
        assert_eq!(
            known_blocker.remediation_bead.as_deref(),
            Some("bd-17c65.10.17.1.2")
        );
        assert!(known_blocker.remote_required);
        assert!(known_blocker.local_fallback_refused);
    }

    #[test]
    fn recommendation_cards_explain_refine_new_and_dirty_checkout_caveats() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.ready = vec![bead("bd-ready", "Refine existing SWA bead", 2)];
        brief.bv = Some(SwarmBriefBvSummary {
            actionable_count: Some(1),
            blocked_count: Some(0),
            in_progress_count: Some(0),
            track_count: None,
            top_picks: vec![SwarmBriefBvPick {
                id: "bd-new".to_owned(),
                title: "Net-new recommendation candidate".to_owned(),
                score_milli: Some(850),
                action_hint: Some("Create a child only after overlap review".to_owned()),
                blocked_by: Vec::new(),
            }],
        });
        brief.dirty_files = vec![SwarmBriefDirtyFile {
            path: "docs/planning.md".to_owned(),
            status: "M".to_owned(),
        }];

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);
        let cards = snapshot.recommendation_cards();

        assert_eq!(
            cards
                .iter()
                .map(|card| (card.candidate_id.as_deref(), card.decision))
                .collect::<Vec<_>>(),
            vec![
                (Some("bd-new"), "new_bead_recommended"),
                (Some("bd-ready"), "refine_existing_bead"),
            ]
        );
        assert!(cards.iter().all(|card| {
            card.evidence_caveats
                .contains(&"dirty_checkout_paths:1".to_owned())
        }));
        assert!(cards.iter().all(|card| {
            card.score_inputs
                .iter()
                .any(|input| input.name == "rank_milli")
        }));
        assert!(cards.iter().all(|card| {
            card.suggested_reservations
                .iter()
                .any(|reservation| reservation.path_pattern == ".beads/issues.jsonl")
        }));

        let json = serde_json::to_value(&snapshot).expect("snapshot serializes");
        assert_eq!(
            json.get("recommendationCards")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn recommendation_cards_rank_safe_work_and_explain_reservations() {
        let safe = candidate(
            "bd-safe",
            "SWA2: conflict-free slice ranking with suggested reservations for swarm next-action",
            "beads_ready",
            Some(2),
        );
        let mut blocked = candidate(
            "bd-owned",
            "SWA2: reserved competing slice",
            "bv_top_pick",
            Some(1),
        );
        blocked.assignee = Some("OtherAgent".to_owned());
        blocked.blocked_by = vec!["bd-upstream".to_owned()];
        blocked.blocked_by_compile_health = true;
        let snapshot = snapshot_with_candidates(vec![blocked, safe]);

        let cards = snapshot.recommendation_cards();

        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].candidate_id.as_deref(), Some("bd-safe"));
        assert_eq!(cards[0].decision, "refine_existing_bead");
        assert!(
            cards[0]
                .suggested_reservations
                .iter()
                .any(|reservation| reservation.path_pattern == "src/core/swarm_next_action.rs")
        );
        assert!(cards[0].suggested_reservations.iter().any(|reservation| {
            reservation.path_pattern == "docs/schemas/ee.swarm_next_action.v1.json"
        }));
        assert!(cards[0].do_not_take_because.is_empty());

        assert_eq!(cards[1].candidate_id.as_deref(), Some("bd-owned"));
        assert_eq!(cards[1].decision, "blocked_by_owner");
        assert!(
            cards[1]
                .do_not_take_because
                .contains(&"candidate_assigned_to:OtherAgent".to_owned())
        );
        assert!(
            cards[1]
                .do_not_take_because
                .contains(&"blocked_by:bd-upstream".to_owned())
        );
        assert!(cards[1].suggested_reservations.is_empty());
    }

    #[test]
    fn recommendation_cards_reject_duplicate_candidate_ids() {
        let snapshot = snapshot_with_candidates(vec![
            candidate("bd-dup", "Duplicate next action", "beads_ready", Some(2)),
            candidate("bd-dup", "Duplicate next action", "beads_ready", Some(2)),
        ]);

        let cards = snapshot.recommendation_cards();

        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].decision, "duplicate_rejected");
        assert_eq!(cards[0].fallback_decision, Some("refine_existing_bead"));
        assert_eq!(
            cards[0].overlap.rejected_duplicate_reason,
            Some("candidate_id_already_present")
        );
        assert!(
            cards[0]
                .do_not_take_because
                .contains(&"candidate_already_appears_in_multiple_sources".to_owned())
        );
    }

    #[test]
    fn recommendation_cards_reuse_recent_verifier_failure_evidence() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.ready = vec![bead("bd-rch", "Needs focused remote proof", 1)];
        brief.dirty_files = vec![SwarmBriefDirtyFile {
            path: "src/db/mod.rs".to_owned(),
            status: "M".to_owned(),
        }];
        let evidence = vec![SwarmNextActionRecentFirstError {
            file: "src/db/mod.rs".to_owned(),
            line: Some(431),
            command_kind: Some("cargo_test".to_owned()),
            command: Some("cargo test --lib focused_remote_proof -- --nocapture".to_owned()),
            command_hash: Some("abc123".to_owned()),
            status: Some("remote_failure".to_owned()),
            degraded_codes: vec!["rch_verify_remote_command_failed".to_owned()],
            source_state_hash: None,
            created_at: None,
            error_codes: Vec::new(),
            remote_required: Some(true),
            local_fallback_refused: false,
            retry_after: None,
            known_blocker: None,
        }];

        let snapshot =
            SwarmNextActionSnapshot::from_swarm_brief_with_verifier_evidence(&brief, &evidence);
        let cards = snapshot.recommendation_cards();

        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].decision, "reuse_recent_evidence");
        assert_eq!(
            cards[0].fallback_decision,
            Some("prefer_static_or_non_cargo_work")
        );
        assert!(cards[0].suggested_reservations.is_empty());
        assert!(
            cards[0]
                .do_not_take_because
                .contains(&"recent_verifier_evidence_available".to_owned())
        );
        assert!(
            cards[0]
                .do_not_take_because
                .contains(&"recent_verifier_command_hash:abc123".to_owned())
        );
        assert!(
            cards[0]
                .do_not_take_because
                .contains(&"recent_verifier_command_kind:cargo_test".to_owned())
        );
        assert!(
            cards[0]
                .do_not_take_because
                .contains(&"recent_verifier_command_target:--lib".to_owned())
        );
        assert!(
            cards[0]
                .proof_obligations
                .contains(&"record_reused_verification_hash_in_closeout".to_owned())
        );
    }

    #[test]
    fn recommendation_cards_reject_overlapping_candidate_titles() {
        let snapshot = snapshot_with_candidates(vec![
            candidate(
                "bd-one",
                "Duplicate verification reuse hook",
                "beads_ready",
                Some(2),
            ),
            candidate(
                "bd-two",
                "duplicate verification-reuse hook",
                "bv_top_pick",
                Some(2),
            ),
        ]);

        let cards = snapshot.recommendation_cards();

        assert_eq!(cards.len(), 2);
        assert!(
            cards
                .iter()
                .all(|card| card.decision == "duplicate_rejected")
        );
        assert!(cards.iter().all(|card| {
            card.overlap.rejected_duplicate_reason == Some("candidate_title_already_present")
        }));
        assert!(
            cards
                .iter()
                .all(|card| card.suggested_reservations.is_empty())
        );
    }

    #[test]
    fn recommendation_cards_emit_no_action_when_evidence_provider_is_missing() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.degraded = vec![degradation(
            SwarmBriefSourceKind::Bv,
            "bv_unavailable",
            "BV robot triage was unavailable.",
            Some("Run bv --robot-triage after repairing bv.".to_owned()),
        )];

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);
        let cards = snapshot.recommendation_cards();

        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].decision, "no_action_recommended");
        assert_eq!(cards[0].confidence, "low");
        assert!(
            cards[0]
                .evidence_caveats
                .contains(&"degraded:bv:bv_unavailable".to_owned())
        );
    }

    #[test]
    fn recommendation_cards_call_out_owner_blocked_compile_health() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.ready = vec![bead("bd-rch", "Needs remote proof", 1)];
        brief.dirty_files = vec![SwarmBriefDirtyFile {
            path: "src/db/mod.rs".to_owned(),
            status: "M".to_owned(),
        }];
        brief.file_reservations = vec![SwarmBriefFileReservation {
            path_pattern: "src/db/*.rs".to_owned(),
            holder: "CloudyHawk".to_owned(),
            exclusive: true,
            expires_at: Some("2026-05-18T10:00:00Z".to_owned()),
        }];

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);
        let cards = snapshot.recommendation_cards();

        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].decision, "blocked_by_owner");
        assert_eq!(cards[0].confidence, "low");
        assert_eq!(
            cards[0].fallback_decision,
            Some("message_owner_before_editing")
        );
        assert!(
            cards[0]
                .proof_obligations
                .contains(&"coordinate_compile_health_blocker_before_rch".to_owned())
        );
        assert!(
            cards[0]
                .evidence_caveats
                .contains(&"compile_health_blocks_rch".to_owned())
        );
    }

    #[test]
    fn recommendation_cards_downgrade_issue_type_epic_rollups() {
        let mut rollup = candidate(
            "bd-epic",
            "SWA2 epic: coordinate crowded checkout fixes",
            "beads_ready",
            Some(1),
        );
        rollup.issue_type = Some("epic".to_owned());
        let concrete = candidate(
            "bd-child",
            "SWA2 child: fix concrete next-action sorting case",
            "beads_ready",
            Some(2),
        );
        let snapshot = snapshot_with_candidates(vec![rollup, concrete]);

        let cards = snapshot.recommendation_cards();

        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].candidate_id.as_deref(), Some("bd-child"));
        assert_eq!(cards[0].decision, "refine_existing_bead");
        assert_eq!(cards[1].candidate_id.as_deref(), Some("bd-epic"));
        assert_eq!(cards[1].decision, "blocked_rollup");
        assert_eq!(cards[1].confidence, "low");
        assert_eq!(
            cards[1].fallback_decision,
            Some("choose_concrete_child_bead")
        );
        assert!(cards[1].suggested_reservations.is_empty());
        assert_eq!(cards[1].overlap.selected_relation, "rollup_not_claimable");
        assert!(
            cards[1]
                .do_not_take_because
                .contains(&"candidate_issue_type:epic".to_owned())
        );
        assert!(
            cards[1]
                .do_not_take_because
                .contains(&"rollup_candidate_not_claimable".to_owned())
        );
        assert!(
            cards[1]
                .proof_obligations
                .contains(&"inspect_claimable_child_bead_before_any_claim".to_owned())
        );
    }

    #[test]
    fn wildcard_path_matching_covers_exact_glob_and_question_patterns() {
        assert!(path_matches_pattern("src/db/mod.rs", "src/db/mod.rs"));
        assert!(path_matches_pattern("src/db/mod.rs", "src/db/*.rs"));
        assert!(path_matches_pattern("src/db/a.rs", "src/db/?.rs"));
        assert!(!path_matches_pattern("src/core/status.rs", "src/db/*.rs"));
    }

    #[test]
    fn work_packet_is_deterministic_read_only_advice_for_safe_candidate() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let snapshot = snapshot_with_candidates(vec![candidate(
            "bd-safe",
            "Implement isolated work packet surface",
            "beads_ready",
            Some(2),
        )]);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let second = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);

        assert_eq!(packet.schema, SWARM_WORK_PACKET_SCHEMA_V1);
        assert_eq!(packet.redaction_status, SWARM_WORK_PACKET_REDACTION_STATUS);
        assert_eq!(packet.packet_id, second.packet_id);
        assert!(packet.packet_id.starts_with("swarm_work_packet_"));
        assert_eq!(packet.observed_state_class, "healthy_small_repo");
        assert_eq!(packet.recommended_action.action, "inspect_and_claim");
        assert_eq!(packet.recommended_action.safe_to_claim, Some(true));
        assert!(packet.tracker_integrity.br_reads_authoritative);
        assert_eq!(packet.tracker_integrity.health, BeadsIntegrityHealth::Ok);
        assert!(packet.mutation_policy.side_effect_free);
        assert!(!packet.mutation_policy.claims_beads);
        assert!(!packet.mutation_policy.reserves_files);
        assert!(!packet.mutation_policy.sends_agent_mail);
        assert!(!packet.mutation_policy.runs_cargo);
        assert!(!packet.mutation_policy.stages_git);
        assert!(!packet.mutation_policy.deletes_files);
    }

    #[test]
    fn work_packet_requires_positive_remote_proof_when_remote_only_required() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut snapshot = snapshot_with_candidates(vec![candidate(
            "bd-safe",
            "Implement isolated work packet surface",
            "beads_ready",
            Some(2),
        )]);
        snapshot.verification.remote_only_required = true;
        snapshot.verification.remote_only_safe = None;
        snapshot.compile_health.safe_to_launch_rch = Some(true);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);

        assert!(packet.rch_proof_posture.remote_only_required);
        assert_eq!(
            packet.rch_proof_posture.safe_to_launch_cargo_verification,
            None
        );
        assert_eq!(packet.recommended_action.action, "prefer_static_docs_work");
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert!(
            packet
                .recommended_action
                .reasons
                .contains(&"rch_remote_verification_required".to_owned())
        );
        assert!(
            packet
                .recommended_action
                .proof_obligations
                .contains(&"collect_rch_status_before_claim".to_owned())
        );
        assert!(
            packet
                .recommended_action
                .suggested_command_actions
                .iter()
                .all(|action| action.command_id != "bead_claim_candidate")
        );

        let gate = packet.claim_gate(None);

        assert_eq!(gate.verdict, "blocked_by_verification");
        assert!(!gate.safe_to_claim);
        assert_eq!(gate.recommended_safe_to_claim, Some(false));
        assert!(gate.claim_command_action.is_none());
        assert!(
            gate.unsafe_reasons
                .contains(&"rch_remote_verification_required".to_owned())
        );
    }

    #[test]
    fn work_packet_blocks_issue_type_rollup_candidates_without_claim_commands() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut rollup = candidate(
            "bd-epic",
            "SWA2 epic: coordinate crowded checkout fixes",
            "beads_ready",
            Some(1),
        );
        rollup.issue_type = Some("epic".to_owned());
        let snapshot = snapshot_with_candidates(vec![rollup]);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);

        assert_eq!(packet.candidates.len(), 1);
        assert_eq!(packet.candidates[0].decision, "blocked_rollup");
        assert!(
            packet.candidates[0]
                .unsafe_reasons
                .contains(&"rollup_candidate_not_claimable".to_owned())
        );
        assert_eq!(packet.recommended_action.action, "blocked_no_action");
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert!(
            packet
                .recommended_action
                .suggested_command_actions
                .iter()
                .all(|action| action.command_id != "bead_claim_candidate")
        );
    }

    #[test]
    fn work_packet_blocks_claim_when_agent_mail_semantic_readiness_fails() {
        let semantic_failure = degradation(
            SwarmBriefSourceKind::AgentMail,
            AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE,
            "Agent Mail semantic readiness failed with healthLevel=green (malformed_sqlite); reservation and inbox reads are not authoritative.",
            Some("Repair Agent Mail storage.".to_owned()),
        );
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief
            .sources
            .push(crate::core::swarm_brief::SwarmBriefSourceSnapshot {
                source: SwarmBriefSourceKind::AgentMail,
                status: crate::core::swarm_brief::SwarmBriefSourceStatus::Degraded,
                freshness: crate::core::swarm_brief::SwarmBriefSourceFreshness::unknown(),
                provenance: crate::core::swarm_brief::SwarmBriefSourceProvenance::local_probe(),
                item_count: 0,
                degraded: vec![semantic_failure.clone()],
            });
        brief.degraded = vec![semantic_failure];
        let snapshot = snapshot_with_candidates(vec![candidate(
            "bd-docs.1",
            "Document a redaction-safe coordination contract",
            "beads_ready",
            Some(2),
        )]);
        let mut snapshot = snapshot;
        snapshot.degraded = brief
            .degraded
            .iter()
            .map(SwarmNextActionDegradation::from_brief)
            .collect();

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let argv = |parts: &[&str]| {
            parts
                .iter()
                .map(|part| (*part).to_owned())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            packet.coordination.agent_mail.status,
            "semantic_readiness_failed"
        );
        assert_eq!(packet.coordination.agent_mail.health_level, Some("green"));
        assert_eq!(
            packet
                .coordination
                .agent_mail
                .semantic_readiness
                .as_ref()
                .map(|readiness| (readiness.status, readiness.reason)),
            Some(("fail", Some("malformed_sqlite")))
        );
        assert_eq!(
            packet.coordination.agent_mail.reservation_authoritative,
            Some(false)
        );
        assert_eq!(
            packet.coordination.agent_mail.inbox_authoritative,
            Some(false)
        );
        assert!(
            packet
                .coordination
                .agent_mail
                .degraded_codes
                .contains(&AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE.to_owned())
        );
        assert!(
            packet
                .coordination
                .agent_mail
                .degraded_codes
                .contains(&AGENT_MAIL_UNAVAILABLE_CODE.to_owned())
        );
        assert_eq!(packet.recommended_action.action, "prefer_static_docs_work");
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert!(
            packet
                .recommended_action
                .reasons
                .contains(&AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE.to_owned())
        );
        assert_eq!(packet.candidates[0].decision, "external_state_required");
        assert!(
            packet.candidates[0]
                .unsafe_reasons
                .contains(&AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE.to_owned())
        );
        assert!(
            packet
                .recommended_action
                .suggested_commands
                .iter()
                .any(|command| command.starts_with("br comments add bd-docs.1"))
        );
        assert!(
            !packet
                .recommended_action
                .suggested_commands
                .iter()
                .any(|command| command.contains("br update"))
        );
        let fallback_kinds = packet
            .coordination
            .agent_mail
            .fallback_actions
            .iter()
            .map(|action| action.kind)
            .collect::<Vec<_>>();
        assert!(
            packet
                .coordination
                .agent_mail
                .fallback_actions
                .iter()
                .all(|action| !action.repair_safety.risk_class.is_empty()
                    && !action.repair_safety.next_action.is_empty()
                    && !action.repair_safety.evidence.is_empty())
        );
        assert_eq!(
            fallback_kinds,
            vec![
                "beads_comment",
                "manual_coordination",
                "retry_later",
                "support_bundle",
                "switch_to_static_work",
            ]
        );
        let support_bundle = packet
            .coordination
            .agent_mail
            .fallback_actions
            .iter()
            .find(|action| action.kind == "support_bundle")
            .expect("support bundle fallback emitted");
        let command_action = support_bundle
            .command_action
            .as_ref()
            .expect("support bundle fallback has structured action");
        assert_eq!(
            support_bundle.command.as_deref(),
            Some(command_action.display_command.as_str())
        );
        assert_eq!(
            command_action.argv,
            argv(&[
                "ee",
                "support",
                "bundle",
                "--workspace",
                ".",
                "--redacted",
                "--dry-run",
                "--json"
            ])
        );
        assert_eq!(command_action.copy_safety, "safe_structured_argv");
        assert!(!command_action.shell_required);
        assert!(!command_action.mutates_state);
        assert_eq!(support_bundle.repair_safety.risk_class, "read_only_probe");
        assert_eq!(support_bundle.repair_safety.next_action, "run_directly");
        assert!(!support_bundle.repair_safety.mutates_external_state);
        let manual_coordination = packet
            .coordination
            .agent_mail
            .fallback_actions
            .iter()
            .find(|action| action.kind == "manual_coordination")
            .expect("manual coordination fallback emitted");
        assert_eq!(
            manual_coordination.repair_safety.risk_class,
            "unavailable_or_manual_only"
        );
        assert_eq!(manual_coordination.repair_safety.next_action, "manual_only");
    }

    #[test]
    fn work_packet_preserves_semantic_readiness_health_level_class() {
        let semantic_failure = degradation(
            SwarmBriefSourceKind::AgentMail,
            AGENT_MAIL_SEMANTIC_READINESS_FAILED_CODE,
            "Agent Mail semantic readiness failed with healthLevel=yellow (index_rebuild_required); reservation and inbox reads are not authoritative.",
            Some("Repair Agent Mail storage.".to_owned()),
        );
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief
            .sources
            .push(crate::core::swarm_brief::SwarmBriefSourceSnapshot {
                source: SwarmBriefSourceKind::AgentMail,
                status: crate::core::swarm_brief::SwarmBriefSourceStatus::Degraded,
                freshness: crate::core::swarm_brief::SwarmBriefSourceFreshness::unknown(),
                provenance: crate::core::swarm_brief::SwarmBriefSourceProvenance::local_probe(),
                item_count: 0,
                degraded: vec![semantic_failure.clone()],
            });
        brief.degraded = vec![semantic_failure];
        let mut snapshot = snapshot_with_candidates(vec![candidate(
            "bd-mail.1",
            "Keep Agent Mail health evidence bounded",
            "beads_ready",
            Some(2),
        )]);
        snapshot.degraded = brief
            .degraded
            .iter()
            .map(SwarmNextActionDegradation::from_brief)
            .collect();

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);

        assert_eq!(packet.coordination.agent_mail.health_level, Some("yellow"));
        assert_eq!(
            packet
                .coordination
                .agent_mail
                .semantic_readiness
                .as_ref()
                .map(|readiness| readiness.reason),
            Some(Some("index_rebuild_required"))
        );
    }

    #[test]
    fn work_packet_source_provenance_surfaces_stale_freshness_as_distinct_status() {
        // bd-2z5ly.5: a beads source that reports `freshness.state == "stale"`
        // (JSONL/DB drift) must produce a packet provenance entry with
        // `status == "stale"`, not be collapsed into the looser "degraded"
        // bucket. This lets agents distinguish "evidence is past TTL" from
        // "the collector itself misbehaved".
        use crate::core::swarm_brief::{
            SwarmBriefSourceFreshness, SwarmBriefSourceProvenance, SwarmBriefSourceSnapshot,
            SwarmBriefSourceStatus,
        };

        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.sources.push(SwarmBriefSourceSnapshot {
            source: SwarmBriefSourceKind::Beads,
            status: SwarmBriefSourceStatus::Degraded,
            freshness: SwarmBriefSourceFreshness {
                observed_at: Some("2026-05-23T00:00:00Z".to_owned()),
                age_seconds: None,
                stale_after_seconds: None,
                state: "stale",
            },
            provenance: SwarmBriefSourceProvenance::local_probe(),
            item_count: 0,
            degraded: Vec::new(),
        });
        brief.sources.push(SwarmBriefSourceSnapshot {
            source: SwarmBriefSourceKind::AgentMail,
            status: SwarmBriefSourceStatus::Unavailable,
            freshness: SwarmBriefSourceFreshness::unknown(),
            provenance: SwarmBriefSourceProvenance::local_probe(),
            item_count: 0,
            degraded: Vec::new(),
        });
        brief.sources.push(SwarmBriefSourceSnapshot {
            source: SwarmBriefSourceKind::Bv,
            status: SwarmBriefSourceStatus::NotConfigured,
            freshness: SwarmBriefSourceFreshness::unknown(),
            provenance: SwarmBriefSourceProvenance::local_probe(),
            item_count: 0,
            degraded: Vec::new(),
        });

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);
        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);

        let by_source = packet
            .source_provenance
            .iter()
            .map(|entry| (entry.source.clone(), entry.status))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(by_source.get("beads"), Some(&"stale"));
        assert_eq!(by_source.get("agent-mail"), Some(&"unavailable"));
        assert_eq!(by_source.get("bv"), Some(&"skipped"));

        // The same brief must hash deterministically, even with freshness in
        // the digest input, so repeated packet builds remain stable.
        let second = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        assert_eq!(packet.packet_id, second.packet_id);
        let beads_entry = packet
            .source_provenance
            .iter()
            .find(|entry| entry.source == "beads")
            .expect("beads provenance entry present");
        assert_eq!(beads_entry.freshness.as_deref(), Some("stale"));
    }

    #[test]
    fn work_packet_preserves_collision_and_rch_blocker_evidence() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.in_progress = vec![SwarmBriefBead {
            id: "bd-peer".to_owned(),
            title: "Peer swarm work".to_owned(),
            status: "in_progress".to_owned(),
            priority: Some(2),
            assignee: Some("BlueLake".to_owned()),
            issue_type: None,
            created_at: None,
            updated_at: None,
            latest_comment_at: None,
            comment_count: 0,
            source_bucket: "in_progress".to_owned(),
        }];
        brief.file_surface_risks = vec![crate::core::swarm_brief::SwarmBriefFileSurfaceRisk {
            path_pattern: "src/core/swarm_*.rs".to_owned(),
            git_status_buckets: vec!["modified".to_owned()],
            reservation_holders: vec!["BlueLake".to_owned()],
            related_bead_ids: vec!["bd-peer".to_owned()],
            severity: "high".to_owned(),
            score: 95,
            risk_factors: vec!["active_exclusive_reservation".to_owned()],
            evidence: vec!["reservation:BlueLake:src/core/swarm_*.rs".to_owned()],
            suggested_commands: vec!["message_owner_before_editing".to_owned()],
        }];
        let mut snapshot = snapshot_with_candidates(vec![candidate(
            "bd-contested",
            "Touch shared swarm collector",
            "bv_top_pick",
            Some(1),
        )]);
        snapshot.compile_health.safe_to_launch_rch = Some(false);
        snapshot.verification.remote_only_safe = Some(false);
        snapshot.degraded = vec![SwarmNextActionDegradation {
            code: "rch_remote_required_fallback_prevented".to_owned(),
            source: "rch".to_owned(),
            severity: "high",
            message: "remote-required fallback prevented local execution".to_owned(),
            repair: Some("wait for RCH topology repair".to_owned()),
        }];

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);

        assert_eq!(packet.observed_state_class, "degraded_mail_rch_topology");
        assert_eq!(packet.recommended_action.action, "prefer_static_docs_work");
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert_eq!(packet.coordination.active_claim_count, 1);
        assert_eq!(packet.coordination.file_collision_count, 1);
        assert_eq!(packet.coordination.file_collisions[0].risk, "high");
        assert_eq!(packet.rch_proof_posture.posture, "degraded_capacity");
        assert!(packet.rch_proof_posture.local_fallback_prevented);
        assert!(
            packet
                .rch_proof_posture
                .blocker_codes
                .contains(&"rch_remote_required_fallback_prevented".to_owned())
        );
        assert!(
            packet
                .recommended_action
                .proof_obligations
                .contains(&"do_not_run_local_cargo_fallback".to_owned())
        );
        let gate = packet.claim_gate(None);
        assert_eq!(
            gate.source_authority.environment_verdict,
            "proof_environment_blocked"
        );
        assert_eq!(
            gate.source_authority.source_test_verdict,
            "environment_blocked_before_source"
        );
        assert_eq!(
            gate.source_authority.remote_verification_admitted,
            Some(false)
        );
        assert_eq!(
            gate.source_authority.local_cargo_fallback_observed,
            Some(false)
        );
    }

    #[test]
    fn work_packet_blocks_claim_for_dirty_reserved_candidate_surface() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.ready = vec![bead(
            "bd-collision",
            "[swarm-work-packet] update swarm next-action ownership classification",
            1,
        )];
        brief.beads.in_progress = vec![SwarmBriefBead {
            id: "bd-owned".to_owned(),
            title: "Peer-owned schema work".to_owned(),
            status: "in_progress".to_owned(),
            priority: Some(2),
            assignee: Some("BlueLake".to_owned()),
            issue_type: None,
            created_at: None,
            updated_at: None,
            latest_comment_at: None,
            comment_count: 0,
            source_bucket: "in_progress".to_owned(),
        }];
        brief.dirty_files = vec![SwarmBriefDirtyFile {
            path: "docs/schemas/ee.swarm_next_action.v1.json".to_owned(),
            status: "M".to_owned(),
        }];
        brief.file_surface_risks = vec![crate::core::swarm_brief::SwarmBriefFileSurfaceRisk {
            path_pattern: "docs/schemas/ee.swarm_next_action.v1.json".to_owned(),
            git_status_buckets: vec!["modified".to_owned()],
            reservation_holders: vec!["BlueLake".to_owned()],
            related_bead_ids: vec!["bd-owned".to_owned()],
            severity: "high".to_owned(),
            score: 95,
            risk_factors: vec!["active_exclusive_reservation".to_owned()],
            evidence: vec![
                "reservation:BlueLake:docs/schemas/ee.swarm_next_action.v1.json".to_owned(),
            ],
            suggested_commands: vec!["message_owner_before_editing".to_owned()],
        }];

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);
        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let candidate = packet
            .candidates
            .iter()
            .find(|candidate| candidate.id == "bd-collision")
            .expect("candidate remains visible");

        assert_eq!(packet.observed_state_class, "crowded_checkout");
        assert_eq!(packet.coordination.active_claim_count, 1);
        assert_eq!(packet.coordination.file_collision_count, 1);
        assert_eq!(
            packet.coordination.file_collisions[0].path_pattern,
            "docs/schemas/ee.swarm_next_action.v1.json"
        );
        assert_eq!(
            packet.coordination.file_collisions[0].owners,
            vec!["BlueLake"]
        );
        assert_eq!(candidate.decision, "unsafe_due_to_conflict");
        assert_eq!(candidate.collision_risk, "high");
        assert!(
            candidate
                .source_refs
                .contains(&"br://bd-collision".to_owned())
        );
        assert!(
            candidate.unsafe_reasons.contains(
                &"dirty_path_overlap:docs/schemas/ee.swarm_next_action.v1.json".to_owned()
            )
        );
        assert!(
            candidate.unsafe_reasons.contains(
                &"file_collision:high:docs/schemas/ee.swarm_next_action.v1.json".to_owned()
            )
        );
        assert!(candidate.unsafe_reasons.contains(
            &"file_collision_owner:BlueLake:docs/schemas/ee.swarm_next_action.v1.json".to_owned()
        ));
        assert!(
            candidate
                .unsafe_reasons
                .contains(&"file_collision_related_bead:bd-owned".to_owned())
        );
        assert_eq!(packet.recommended_action.action, "coordinate_before_claim");
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert!(
            packet
                .recommended_action
                .suggested_command_actions
                .iter()
                .all(|action| action.command_id != "bead_claim_candidate")
        );
        assert!(
            !packet
                .recommended_action
                .suggested_commands
                .iter()
                .any(|command| command.contains("br update"))
        );
        let packet_json = serde_json::to_string(&packet).expect("packet serializes");
        assert!(packet_json.contains("BlueLake"));
        assert!(packet_json.contains("docs/schemas/ee.swarm_next_action.v1.json"));
        assert_eq!(packet.redaction_status, SWARM_WORK_PACKET_REDACTION_STATUS);
        assert!(!packet_json.contains("raw mail body"));
    }

    #[test]
    fn work_packet_normalizes_verifier_topology_refusal_into_rch_posture() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.beads.ready = vec![bead("bd-proof", "Needs RCH verifier evidence", 2)];
        let evidence = verifier_evidence_from_json(&serde_json::json!({
            "schema": "ee.rch.verify.v1",
            "status": "rch_environment_failure",
            "command_text": "cargo test --test rch_verify_contract",
            "command_kind": "cargo_test",
            "command_hash": "cd825533cce8c288",
            "remote_required": true,
            "retry_after": "2026-05-23T07:00:00Z",
            "error_codes": ["RCH-E327"],
            "degraded_codes": [
                "rch_verify_remote_command_failed",
                "rch_verify_topology_blocked",
                "rch_verify_local_fallback_refused",
                "rch_verify_remote_marker_missing"
            ]
        }));

        let packet = SwarmWorkPacket::from_swarm_brief_with_verifier_evidence(&brief, &evidence);

        assert_eq!(packet.rch_proof_posture.posture, "topology_blocked");
        assert!(packet.rch_proof_posture.source_enabled);
        assert!(packet.rch_proof_posture.remote_only_required);
        assert_eq!(
            packet.rch_proof_posture.safe_to_launch_cargo_verification,
            Some(false)
        );
        assert!(packet.rch_proof_posture.local_fallback_prevented);
        assert!(
            packet
                .rch_proof_posture
                .blocker_codes
                .contains(&"rch_worker_topology_blocked".to_owned())
        );
        assert!(
            packet
                .rch_proof_posture
                .blocker_codes
                .contains(&"rch_remote_required_fallback_prevented".to_owned())
        );
        assert_eq!(
            packet.rch_proof_posture.retry_after.as_deref(),
            Some("2026-05-23T07:00:00Z")
        );
        assert_eq!(packet.rch_proof_posture.known_blockers.len(), 1);
        assert_eq!(packet.rch_proof_posture.known_blockers[0].code, "RCH-E327");
        assert!(
            packet.rch_proof_posture.known_blockers[0]
                .fingerprint
                .starts_with("sha256:")
        );
        assert_eq!(
            packet.rch_proof_posture.known_blockers[0]
                .command_hash
                .as_deref(),
            Some("cd825533cce8c288")
        );
        assert_eq!(
            packet.rch_proof_posture.known_blockers[0]
                .remediation_bead
                .as_deref(),
            Some("bd-17c65.10.17.1.2")
        );
        assert_eq!(
            packet.verification.required_commands[0].last_outcome,
            "environment_blocked"
        );
        assert_eq!(
            packet.verification.required_commands[0]
                .last_command_hash
                .as_deref(),
            Some("cd825533cce8c288")
        );
        assert_eq!(packet.recommended_action.action, "prefer_static_docs_work");
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert!(
            packet
                .recommended_action
                .proof_obligations
                .contains(&"do_not_run_local_cargo_fallback".to_owned())
        );
    }

    #[test]
    fn work_packet_emits_structured_command_actions_for_agent_commands() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let snapshot = snapshot_with_candidates(vec![candidate(
            "bd-safe",
            "Emit safe command argv",
            "beads_ready",
            Some(2),
        )]);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let argv = |parts: &[&str]| {
            parts
                .iter()
                .map(|part| (*part).to_owned())
                .collect::<Vec<_>>()
        };

        let display_commands = packet
            .recommended_action
            .suggested_command_actions
            .iter()
            .map(|action| action.display_command.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            packet.recommended_action.suggested_commands,
            display_commands
        );

        let show_candidate = packet
            .recommended_action
            .suggested_command_actions
            .iter()
            .find(|action| action.command_id == "bead_show_candidate")
            .expect("show candidate action emitted");
        assert_eq!(
            show_candidate.argv,
            argv(&["br", "show", "bd-safe", "--json"])
        );
        assert_eq!(show_candidate.copy_safety, "safe_structured_argv");
        assert!(!show_candidate.shell_required);
        assert!(!show_candidate.mutates_state);

        let claim_candidate = packet
            .recommended_action
            .suggested_command_actions
            .iter()
            .find(|action| action.command_id == "bead_claim_candidate")
            .expect("claim candidate action emitted");
        assert_eq!(
            claim_candidate.argv,
            argv(&[
                "br",
                "update",
                "bd-safe",
                "--status",
                "in_progress",
                "--json"
            ])
        );
        assert!(claim_candidate.mutates_state);

        let rch_command = packet
            .verification
            .required_commands
            .iter()
            .find(|command| command.command_id == "cargo_check_all_targets")
            .expect("RCH command emitted");
        assert_eq!(
            rch_command.command_action.command_id,
            rch_command.command_id
        );
        assert_eq!(
            rch_command.command_action.argv,
            argv(&[
                "env",
                "RCH_REQUIRE_REMOTE=1",
                "scripts/rch_verify.sh",
                "--",
                "cargo",
                "check",
                "--all-targets"
            ])
        );
        assert_eq!(
            rch_command.command_action.copy_safety,
            "safe_structured_argv"
        );
        assert!(!rch_command.command_action.shell_required);
        assert!(rch_command.command_action.mutates_state);

        for command in packet
            .verification
            .required_commands
            .iter()
            .chain(packet.verification.static_checks.iter())
        {
            assert_eq!(command.command_action.command_id, command.command_id);
            assert_eq!(command.command_action.copy_safety, "safe_structured_argv");
            assert!(!command.command_action.shell_required);
            assert!(!command.command_action.argv.is_empty());
        }
    }

    #[test]
    fn work_packet_claim_gate_allows_claim_only_for_safe_candidate() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let snapshot = snapshot_with_candidates(vec![candidate(
            "bd-safe",
            "Gate safe work-packet candidate",
            "beads_ready",
            Some(2),
        )]);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let gate = packet.claim_gate(None);
        let second = packet.claim_gate(None);

        assert_eq!(gate.schema, SWARM_WORK_PACKET_CLAIM_GATE_SCHEMA_V1);
        assert_eq!(gate.gate_id, second.gate_id);
        assert_eq!(gate.packet_id, packet.packet_id);
        assert_eq!(gate.verdict, "safe_to_claim");
        assert!(gate.safe_to_claim);
        assert_eq!(
            gate.selected_candidate
                .as_ref()
                .map(|candidate| candidate.id.as_str()),
            Some("bd-safe")
        );
        assert!(gate.unsafe_reasons.is_empty());
        assert_eq!(gate.recommended_safe_to_claim, Some(true));
        assert!(
            gate.next_command_actions
                .iter()
                .all(|action| !action.mutates_state)
        );
        assert!(
            gate.next_command_actions
                .iter()
                .any(|action| action.command_id == "bead_show_candidate")
        );
        assert_eq!(
            gate.claim_command_action
                .as_ref()
                .map(|action| action.command_id),
            Some("bead_claim_candidate")
        );
        assert_eq!(
            gate.source_authority.environment_verdict,
            "remote_verification_admitted"
        );
        assert_eq!(gate.source_authority.source_test_verdict, "not_evaluated");
        assert_eq!(
            gate.source_authority.remote_verification_admitted,
            Some(true)
        );
        assert_eq!(
            gate.source_authority.local_cargo_fallback_observed,
            Some(false)
        );
    }

    #[test]
    fn work_packet_claim_gate_source_authority_uses_attestation_summary() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let snapshot = snapshot_with_candidates(vec![candidate(
            "bd-safe",
            "Gate safe work-packet candidate",
            "beads_ready",
            Some(2),
        )]);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let summary = work_packet_claim_gate_attestation_summary(&packet);
        let gate = packet.claim_gate(None);

        assert!(summary.safe_to_claim);
        assert_eq!(
            summary.environment_verdict,
            EnvironmentAttestationVerdict::RemoteVerificationAdmitted
        );
        assert_eq!(
            summary.source_test_verdict,
            EnvironmentAttestationSourceTestVerdict::NotEvaluated
        );
        assert_eq!(summary.remote_verification_admitted, Some(true));
        assert!(!summary.local_cargo_fallback_observed);
        assert_eq!(
            gate.source_authority.environment_verdict,
            environment_attestation_verdict_label(summary.environment_verdict)
        );
        assert_eq!(
            gate.source_authority.source_test_verdict,
            environment_attestation_source_test_verdict_label(summary.source_test_verdict)
        );
        assert_eq!(
            gate.source_authority.remote_verification_admitted,
            summary.remote_verification_admitted
        );
        assert_eq!(
            gate.source_authority.local_cargo_fallback_observed,
            Some(summary.local_cargo_fallback_observed)
        );
    }

    #[test]
    fn work_packet_claim_gate_attestation_summary_marks_local_cargo_bypass() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut snapshot = snapshot_with_candidates(vec![candidate(
            "bd-safe",
            "Gate safe work-packet candidate",
            "beads_ready",
            Some(2),
        )]);
        snapshot.degraded = vec![SwarmNextActionDegradation {
            code: "local_cargo_bypass_detected".to_owned(),
            source: "local-cargo-tripwire".to_owned(),
            severity: "high",
            message: "Local Cargo process observed in a remote-only lane.".to_owned(),
            repair: Some("Stop local Cargo and rerun through scripts/rch_verify.sh.".to_owned()),
        }];

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let summary = work_packet_claim_gate_attestation_summary(&packet);
        let gate = packet.claim_gate(None);

        assert!(!summary.safe_to_claim);
        assert_eq!(
            summary.environment_verdict,
            EnvironmentAttestationVerdict::LocalCargoBypassDetected
        );
        assert!(summary.local_cargo_fallback_observed);
        assert_eq!(
            gate.source_authority.environment_verdict,
            "local_cargo_bypass_detected"
        );
        assert_eq!(gate.source_authority.source_test_verdict, "not_evaluated");
        assert_eq!(
            gate.source_authority.local_cargo_fallback_observed,
            Some(true)
        );
    }

    #[test]
    fn work_packet_claim_gate_scopes_conflicts_to_candidate_paths() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.file_surface_risks = vec![crate::core::swarm_brief::SwarmBriefFileSurfaceRisk {
            path_pattern: "tests/**".to_owned(),
            git_status_buckets: Vec::new(),
            reservation_holders: Vec::new(),
            related_bead_ids: vec!["bd-other-tests".to_owned()],
            severity: "high".to_owned(),
            score: 100,
            risk_factors: vec!["ready_bead_likely_surface".to_owned()],
            evidence: vec!["bead:bd-other-tests:ready:touch test suite".to_owned()],
            suggested_commands: vec!["br show bd-other-tests --json".to_owned()],
        }];
        let mut snapshot = snapshot_with_candidates(vec![candidate(
            "bd-pack",
            "Improve context pack export",
            "beads_ready",
            Some(1),
        )]);
        snapshot.checkout.dirty_path_count = 1;
        snapshot.checkout.dirty_paths = vec!["-".to_owned()];

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let candidate = packet
            .candidates
            .iter()
            .find(|candidate| candidate.id == "bd-pack")
            .expect("pack candidate present");

        assert_eq!(packet.coordination.file_collision_count, 1);
        assert_eq!(
            packet.coordination.file_collisions[0].path_pattern,
            "tests/**"
        );
        assert_eq!(candidate.decision, "safe_to_claim");
        assert!(
            !candidate
                .unsafe_reasons
                .contains(&"high_risk_dirty_surface:tests/**".to_owned())
        );
        assert!(
            !candidate
                .unsafe_reasons
                .contains(&"dirty_checkout_path_count:1".to_owned())
        );

        let gate = packet.claim_gate(Some("bd-pack"));

        assert_eq!(packet.recommended_action.safe_to_claim, Some(true));
        assert_eq!(gate.verdict, "safe_to_claim");
        assert!(gate.safe_to_claim);
        assert!(gate.unsafe_reasons.is_empty());
        assert_eq!(
            gate.claim_command_action
                .as_ref()
                .map(|action| action.command_id),
            Some("bead_claim_candidate")
        );
    }

    #[test]
    fn work_packet_claim_gate_uses_candidate_scoped_recommendation_safety() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut blocked = candidate("bd-blocked", "Blocked candidate", "beads_ready", Some(1));
        blocked.blocked_by = vec!["bd-parent".to_owned()];
        let snapshot = snapshot_with_candidates(vec![
            candidate(
                "bd-safe",
                "Gate safe work-packet candidate",
                "beads_ready",
                Some(2),
            ),
            blocked,
        ]);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let gate = packet.claim_gate(Some("bd-blocked"));

        assert_eq!(packet.recommended_action.safe_to_claim, Some(true));
        assert_eq!(gate.verdict, "blocked_by_dependency");
        assert!(!gate.safe_to_claim);
        assert_eq!(gate.recommended_safe_to_claim, Some(false));
        assert!(gate.claim_command_action.is_none());
        assert!(
            gate.unsafe_reasons
                .contains(&"candidate_decision:blocked_by_dependency".to_owned())
        );
    }

    #[test]
    fn work_packet_claim_gate_requires_requested_candidate_to_match_packet_recommendation() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let snapshot = snapshot_with_candidates(vec![
            candidate(
                "bd-one",
                "Recommended safe candidate",
                "beads_ready",
                Some(1),
            ),
            candidate("bd-two", "Different safe candidate", "beads_ready", Some(2)),
        ]);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let gate = packet.claim_gate(Some("bd-two"));

        assert_eq!(
            packet.recommended_action.candidate_id.as_deref(),
            Some("bd-one")
        );
        assert_eq!(packet.recommended_action.safe_to_claim, Some(true));
        assert_eq!(gate.verdict, "coordinate_first");
        assert!(!gate.safe_to_claim);
        assert_eq!(gate.recommended_safe_to_claim, Some(false));
        assert!(gate.claim_command_action.is_none());
        assert!(
            gate.unsafe_reasons
                .contains(&"packet_recommendation_candidate_mismatch:bd-one:bd-two".to_owned())
        );
    }

    #[test]
    fn work_packet_claim_gate_specific_unsafe_candidate_never_emits_claim_action() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut blocked = candidate("bd-blocked", "Blocked candidate", "beads_ready", Some(1));
        blocked.blocked_by = vec!["bd-parent".to_owned()];
        let snapshot = snapshot_with_candidates(vec![blocked]);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let gate = packet.claim_gate(Some("bd-blocked"));

        assert_eq!(gate.requested_candidate_id.as_deref(), Some("bd-blocked"));
        assert_eq!(gate.verdict, "blocked_by_dependency");
        assert!(!gate.safe_to_claim);
        assert!(gate.claim_command_action.is_none());
        assert!(
            gate.unsafe_reasons
                .contains(&"candidate_decision:blocked_by_dependency".to_owned())
        );
        assert!(
            gate.next_command_actions
                .iter()
                .all(|action| !action.mutates_state)
        );
    }

    #[test]
    fn work_packet_downgrades_candidates_when_beads_reads_are_not_authoritative() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let snapshot = snapshot_with_candidates(vec![candidate(
            "bd-stale",
            "Claim only after tracker reconciliation",
            "beads_ready",
            Some(2),
        )]);
        let merge_artifact_paths = Vec::new();
        let tracker_integrity = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_path: ".beads/issues.jsonl",
            db_path: ".beads/beads.db",
            jsonl_record_count: 12,
            db_record_count: 12,
            auto_import_enabled: true,
            external_changes_pending_import: true,
            dirty_issue_count: 1,
            merge_artifact_paths: &merge_artifact_paths,
            jsonl_parse_error: None,
        });

        let packet = SwarmWorkPacket::from_brief_and_next_action_with_tracker_integrity(
            &brief,
            &snapshot,
            tracker_integrity,
        );

        assert_eq!(
            packet.tracker_integrity.health,
            BeadsIntegrityHealth::ExternalChangesPendingImport
        );
        assert!(!packet.tracker_integrity.br_reads_authoritative);
        assert_eq!(packet.recommended_action.action, "coordinate_before_claim");
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert_eq!(packet.candidates[0].decision, "external_state_required");
        assert!(packet.candidates[0].unsafe_reasons.contains(
            &"beads_tracker_not_authoritative:external_changes_pending_import".to_owned()
        ));
        assert!(
            packet
                .recommended_action
                .proof_obligations
                .contains(&"repair_beads_tracker_before_claim".to_owned())
        );
        assert!(
            packet
                .recommended_action
                .suggested_commands
                .contains(&"br doctor --json --no-db".to_owned())
        );
        assert!(
            !packet
                .recommended_action
                .suggested_commands
                .iter()
                .any(|command| command.contains("br update"))
        );
    }

    #[test]
    fn work_packet_keeps_metadata_only_pending_import_claimable() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let snapshot = snapshot_with_candidates(vec![candidate(
            "bd-metadata-only",
            "Metadata-only stale marker should not block claims",
            "beads_ready",
            Some(1),
        )]);
        let merge_artifact_paths = Vec::new();
        let tracker_integrity = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_path: ".beads/issues.jsonl",
            db_path: ".beads/beads.db",
            jsonl_record_count: 42,
            db_record_count: 42,
            auto_import_enabled: true,
            external_changes_pending_import: true,
            dirty_issue_count: 0,
            merge_artifact_paths: &merge_artifact_paths,
            jsonl_parse_error: None,
        });

        let packet = SwarmWorkPacket::from_brief_and_next_action_with_tracker_integrity(
            &brief,
            &snapshot,
            tracker_integrity,
        );
        let gate = packet.claim_gate(Some("bd-metadata-only"));

        assert_eq!(
            packet.tracker_integrity.health,
            BeadsIntegrityHealth::ExternalChangesPendingImport
        );
        assert!(packet.tracker_integrity.br_reads_authoritative);
        assert_eq!(packet.candidates[0].decision, "safe_to_claim");
        assert!(
            !packet.candidates[0]
                .unsafe_reasons
                .iter()
                .any(|reason| reason.starts_with("beads_tracker_not_authoritative:"))
        );
        assert_eq!(gate.source_authority.tracker_authoritative, true);
        assert_eq!(gate.verdict, "safe_to_claim");
        assert!(gate.safe_to_claim);
        assert!(gate.claim_command_action.is_some());
    }

    #[test]
    fn work_packet_keeps_ready_rows_with_in_progress_status_unclaimable() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut owned_ready_row = candidate(
            "bd-owned-ready",
            "Ready source row already owned by another lane",
            "beads_ready",
            Some(2),
        );
        owned_ready_row.status = "in_progress".to_owned();
        owned_ready_row.assignee = Some("cc-cass".to_owned());
        let snapshot = snapshot_with_candidates(vec![owned_ready_row]);
        let merge_artifact_paths = Vec::new();
        let tracker_integrity = compose_integrity_report(BeadsIntegrityInputs {
            jsonl_path: ".beads/issues.jsonl",
            db_path: ".beads/beads.db",
            jsonl_record_count: 41,
            db_record_count: 42,
            auto_import_enabled: true,
            external_changes_pending_import: true,
            dirty_issue_count: 1,
            merge_artifact_paths: &merge_artifact_paths,
            jsonl_parse_error: None,
        });

        let packet = SwarmWorkPacket::from_brief_and_next_action_with_tracker_integrity(
            &brief,
            &snapshot,
            tracker_integrity,
        );

        assert_eq!(packet.candidates.len(), 1);
        let candidate = &packet.candidates[0];
        assert_eq!(candidate.source, "beads_ready");
        assert_eq!(candidate.status, "in_progress");
        assert_eq!(candidate.assignee.as_deref(), Some("cc-cass"));
        assert_eq!(candidate.decision, "already_owned");
        assert!(
            candidate
                .unsafe_reasons
                .contains(&"candidate_assigned_to:cc-cass".to_owned())
        );
        assert!(
            candidate
                .unsafe_reasons
                .contains(&"beads_tracker_not_authoritative:db_jsonl_count_mismatch".to_owned())
        );
        assert!(!packet.tracker_integrity.br_reads_authoritative);
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert_eq!(packet.recommended_action.action, "blocked_no_action");
        assert!(
            packet
                .recommended_action
                .proof_obligations
                .contains(&"repair_beads_tracker_before_claim".to_owned())
        );
        assert!(
            packet
                .recommended_action
                .suggested_command_actions
                .iter()
                .all(|action| action.command_id != "bead_claim_candidate")
        );
        assert!(
            packet
                .recommended_action
                .suggested_commands
                .contains(&"br doctor --json --no-db".to_owned())
        );
        assert!(packet.recommended_action.suggested_commands.iter().any(
            |command| command == "br --no-auto-import --allow-stale show bd-owned-ready --json"
        ));
    }

    #[test]
    fn work_packet_marks_owned_candidates_unclaimable_without_claim_command() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut owned_candidate = candidate(
            "bd-owned",
            "Owned candidate should remain visible",
            "beads_ready",
            Some(2),
        );
        owned_candidate.assignee = Some("BlueLake".to_owned());
        let snapshot = snapshot_with_candidates(vec![owned_candidate]);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);

        assert_eq!(packet.candidates.len(), 1);
        let candidate = &packet.candidates[0];
        assert_eq!(candidate.decision, "already_owned");
        assert_eq!(candidate.collision_risk, "high");
        assert!(
            candidate
                .unsafe_reasons
                .contains(&"candidate_assigned_to:BlueLake".to_owned())
        );
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert_eq!(packet.recommended_action.action, "coordinate_before_claim");
        assert!(
            packet
                .recommended_action
                .suggested_command_actions
                .iter()
                .all(|action| action.command_id != "bead_claim_candidate")
        );
    }

    #[test]
    fn work_packet_blocks_bv_picks_with_blocked_or_deferred_beads_status() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut blocked_candidate = candidate(
            "bd-blocked-rollup",
            "BV stale rollup should not be claimable",
            "bv_top_pick",
            Some(1),
        );
        blocked_candidate.status = "blocked".to_owned();
        let mut deferred_candidate = candidate(
            "bd-deferred-rollup",
            "BV deferred rollup should wait for external state",
            "bv_top_pick",
            Some(2),
        );
        deferred_candidate.status = "deferred".to_owned();
        let snapshot = snapshot_with_candidates(vec![blocked_candidate, deferred_candidate]);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let by_id = packet
            .candidates
            .iter()
            .map(|candidate| (candidate.id.as_str(), candidate))
            .collect::<BTreeMap<_, _>>();
        let blocked = by_id
            .get("bd-blocked-rollup")
            .expect("blocked candidate visible");
        let deferred = by_id
            .get("bd-deferred-rollup")
            .expect("deferred candidate visible");

        assert_eq!(blocked.source, "bv_top_pick");
        assert_eq!(blocked.decision, "blocked_by_dependency");
        assert!(
            blocked
                .source_refs
                .contains(&"br://bd-blocked-rollup".to_owned())
        );
        assert!(
            blocked
                .source_refs
                .contains(&"bv://top-pick/bd-blocked-rollup".to_owned())
        );
        assert_eq!(deferred.decision, "external_state_required");
        assert!(
            deferred
                .source_refs
                .contains(&"br://bd-deferred-rollup".to_owned())
        );
        assert!(
            deferred
                .source_refs
                .contains(&"bv://top-pick/bd-deferred-rollup".to_owned())
        );
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert_eq!(packet.recommended_action.action, "blocked_no_action");
        assert!(
            packet
                .recommended_action
                .suggested_command_actions
                .iter()
                .all(|action| action.command_id != "bead_claim_candidate")
        );
    }

    #[test]
    fn work_packet_blocks_bv_false_ready_parent_from_brief_sources() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut blocked_parent = bead("bd-blocked-parent", "Blocked rollup parent", 1);
        blocked_parent.status = "blocked".to_owned();
        blocked_parent.assignee = Some("cod-core".to_owned());
        blocked_parent.source_bucket = "blocked".to_owned();
        brief.beads.blocked = vec![blocked_parent];
        brief.bv = Some(SwarmBriefBvSummary {
            actionable_count: Some(1),
            blocked_count: Some(1),
            in_progress_count: Some(0),
            track_count: None,
            top_picks: vec![SwarmBriefBvPick {
                id: "bd-blocked-parent".to_owned(),
                title: "Blocked rollup parent".to_owned(),
                score_milli: Some(950),
                action_hint: Some("br update bd-blocked-parent --status in_progress".to_owned()),
                blocked_by: Vec::new(),
            }],
        });

        let snapshot = SwarmNextActionSnapshot::from_swarm_brief(&brief);

        assert_eq!(snapshot.inputs.ready_bead_count, 0);
        assert_eq!(snapshot.inputs.blocked_bead_count, 1);
        assert_eq!(snapshot.candidates.len(), 1);
        assert_eq!(snapshot.candidates[0].source, "bv_top_pick");
        assert_eq!(snapshot.candidates[0].status, "blocked");
        assert_eq!(snapshot.candidates[0].assignee.as_deref(), Some("cod-core"));

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let candidate = &packet.candidates[0];

        assert_eq!(candidate.id, "bd-blocked-parent");
        assert_eq!(candidate.source, "bv_top_pick");
        assert_eq!(candidate.status, "blocked");
        assert_eq!(candidate.decision, "blocked_by_dependency");
        assert!(
            candidate
                .source_refs
                .contains(&"br://bd-blocked-parent".to_owned())
        );
        assert!(
            candidate
                .source_refs
                .contains(&"bv://top-pick/bd-blocked-parent".to_owned())
        );
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert_eq!(packet.recommended_action.action, "blocked_no_action");
        assert!(
            packet
                .recommended_action
                .suggested_commands
                .iter()
                .all(|command| !command.contains("br update"))
        );
        assert!(
            packet
                .recommended_action
                .suggested_command_actions
                .iter()
                .all(|action| action.command_id != "bead_claim_candidate")
        );
    }

    #[test]
    fn work_packet_marks_release_operator_lanes_unclaimable() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let mut release_candidate = candidate(
            "bd-release",
            "publish-dep:fnx-runtime 0.1.0 crates.io workflow",
            "bv_top_pick",
            Some(2),
        );
        release_candidate.score_milli = Some(950);
        let local_candidate = candidate(
            "bd-safe-local",
            "docs: local schema cleanup",
            "beads_ready",
            Some(2),
        );
        let snapshot = snapshot_with_candidates(vec![release_candidate, local_candidate]);

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);
        let candidates = packet
            .candidates
            .iter()
            .map(|candidate| (candidate.id.as_str(), candidate))
            .collect::<BTreeMap<_, _>>();
        let release = candidates
            .get("bd-release")
            .expect("release lane remains visible");

        assert_eq!(
            packet
                .candidates
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            vec!["bd-release", "bd-safe-local"]
        );
        assert_eq!(release.source, "bv_top_pick");
        assert_eq!(release.decision, "release_operator_required");
        assert!(
            release
                .unsafe_reasons
                .contains(&"release_operator_required:dependency_publish".to_owned())
        );
        assert!(
            release
                .unsafe_reasons
                .contains(&"release_operator_required:crates_io_publish".to_owned())
        );
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert_eq!(packet.recommended_action.action, "blocked_no_action");
        assert!(
            packet
                .recommended_action
                .suggested_commands
                .iter()
                .all(|command| !command.contains("br update"))
        );
        assert!(
            packet
                .recommended_action
                .suggested_command_actions
                .iter()
                .all(|action| action.command_id != "bead_claim_candidate")
        );
    }

    #[test]
    fn work_packet_uses_stale_thresholds_before_owned_claims() {
        let brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        let proposal = |bead_id: &str,
                        decision: &'static str,
                        evidence: &[&str]|
         -> SwarmNextActionStaleWorkProposal {
            SwarmNextActionStaleWorkProposal {
                bead_id: bead_id.to_owned(),
                title: format!("Candidate {bead_id}"),
                assignee: Some("QuietHill".to_owned()),
                decision,
                confidence: if decision == "reopenSuggested" {
                    "medium"
                } else {
                    "high"
                },
                evidence: evidence.iter().map(|entry| (*entry).to_owned()).collect(),
                caveats: Vec::new(),
                suggested_commands: Vec::new(),
            }
        };

        let mut active_candidate = candidate(
            "bd-active",
            "Fresh owned candidate with active reservation",
            "bv_top_pick",
            Some(2),
        );
        active_candidate.status = "in_progress".to_owned();
        active_candidate.assignee = Some("BlueLake".to_owned());
        let mut active_snapshot = snapshot_with_candidates(vec![active_candidate]);
        active_snapshot.stale_work_proposals = vec![proposal(
            "bd-active",
            "leaveAloneActive",
            &["active_reservation_holder:BlueLake:src/search/**"],
        )];

        let active_packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &active_snapshot);
        let active = &active_packet.candidates[0];
        assert_eq!(active.decision, "already_owned");
        assert_eq!(
            active_packet.recommended_action.action,
            "coordinate_before_claim"
        );
        assert!(
            active_packet
                .recommended_action
                .suggested_command_actions
                .iter()
                .all(|action| action.command_id != "bead_claim_candidate")
        );

        let mut stale_candidate = candidate(
            "bd-stale",
            "Inactive owned candidate with missing activity signals",
            "bv_top_pick",
            Some(2),
        );
        stale_candidate.status = "in_progress".to_owned();
        stale_candidate.assignee = Some("QuietHill".to_owned());
        let mut stale_snapshot = snapshot_with_candidates(vec![stale_candidate]);
        stale_snapshot.stale_work_proposals = vec![proposal(
            "bd-stale",
            "reopenSuggested",
            &[
                "no_matching_active_reservation",
                "no_recent_commit_mentions_bead",
                "no_mail_thread_mentions_bead",
            ],
        )];

        let stale_packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &stale_snapshot);
        let stale = &stale_packet.candidates[0];
        assert_eq!(stale.decision, "stale_but_reclaimable");
        assert_eq!(stale_packet.recommended_action.action, "reopen_stale_work");
        assert_eq!(stale_packet.recommended_action.safe_to_claim, Some(false));
        assert!(stale.source_refs.contains(&"br://bd-stale".to_owned()));
        for reason in [
            "no_matching_active_reservation",
            "no_recent_commit_mentions_bead",
            "no_mail_thread_mentions_bead",
        ] {
            assert!(
                stale.stale_reasons.contains(&reason.to_owned()),
                "missing stale reason {reason}"
            );
        }
        assert!(
            stale_packet
                .recommended_action
                .suggested_command_actions
                .iter()
                .all(|action| action.command_id != "bead_claim_candidate")
        );

        let mut blocked_candidate = candidate(
            "bd-blocked",
            "Blocked stale candidate should not be reclaimed",
            "bv_top_pick",
            Some(2),
        );
        blocked_candidate.status = "in_progress".to_owned();
        blocked_candidate.assignee = Some("QuietHill".to_owned());
        blocked_candidate.blocked_by = vec!["bd-parent".to_owned()];
        let mut blocked_snapshot = snapshot_with_candidates(vec![blocked_candidate]);
        blocked_snapshot.stale_work_proposals = vec![proposal(
            "bd-blocked",
            "reopenSuggested",
            &[
                "no_matching_active_reservation",
                "no_recent_commit_mentions_bead",
                "blocked_by:bd-parent",
            ],
        )];

        let blocked_packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &blocked_snapshot);
        assert_eq!(
            blocked_packet.candidates[0].decision,
            "blocked_by_dependency"
        );
        assert_eq!(
            blocked_packet.recommended_action.action,
            "blocked_no_action"
        );
    }

    #[test]
    fn work_packet_marks_dirty_or_reserved_candidates_unsafe_without_claim_command() {
        let mut brief = SwarmBriefReport::empty(Path::new("/tmp/project"));
        brief.file_surface_risks = vec![crate::core::swarm_brief::SwarmBriefFileSurfaceRisk {
            path_pattern: "src/core/swarm_*.rs".to_owned(),
            git_status_buckets: vec!["modified".to_owned()],
            reservation_holders: vec!["BlueLake".to_owned()],
            related_bead_ids: vec!["bd-peer".to_owned()],
            severity: "high".to_owned(),
            score: 95,
            risk_factors: vec!["active_exclusive_reservation".to_owned()],
            evidence: vec!["reservation:BlueLake:src/core/swarm_*.rs".to_owned()],
            suggested_commands: vec!["message_owner_before_editing".to_owned()],
        }];
        let mut snapshot = snapshot_with_candidates(vec![candidate(
            "bd-contested",
            "Touch shared swarm collector",
            "beads_ready",
            Some(2),
        )]);
        snapshot.checkout.dirty_path_count = 1;
        snapshot.checkout.dirty_paths = vec!["src/core/swarm_next_action.rs".to_owned()];

        let packet = SwarmWorkPacket::from_brief_and_next_action(&brief, &snapshot);

        assert_eq!(packet.candidates.len(), 1);
        let candidate = &packet.candidates[0];
        assert_eq!(candidate.decision, "unsafe_due_to_conflict");
        assert_eq!(candidate.collision_risk, "medium");
        for reason in [
            "dirty_checkout_path_count:1",
            "high_risk_dirty_surface:src/core/swarm_*.rs",
            "reservation_collision:src/core/swarm_*.rs",
            "related_bead_collision:src/core/swarm_*.rs",
        ] {
            assert!(
                candidate.unsafe_reasons.contains(&reason.to_owned()),
                "missing unsafe reason {reason}"
            );
        }
        assert_eq!(packet.recommended_action.safe_to_claim, Some(false));
        assert_eq!(packet.recommended_action.action, "coordinate_before_claim");
        assert!(
            packet
                .recommended_action
                .suggested_command_actions
                .iter()
                .all(|action| action.command_id != "bead_claim_candidate")
        );
    }

    fn snapshot_with_candidates(
        candidates: Vec<SwarmNextActionCandidate>,
    ) -> SwarmNextActionSnapshot {
        SwarmNextActionSnapshot {
            schema: SWARM_NEXT_ACTION_SCHEMA_V1,
            workspace: "/tmp/project".to_owned(),
            redaction_status: SWARM_NEXT_ACTION_REDACTION_STATUS,
            inputs: SwarmNextActionInputSummary {
                source_count: 1,
                ready_bead_count: candidates.len(),
                in_progress_bead_count: 0,
                blocked_bead_count: 0,
                bv_top_pick_count: 0,
            },
            candidates,
            stale_work_proposals: Vec::new(),
            coordination: SwarmNextActionCoordinationSummary {
                active_reservation_count: 0,
                reservation_holders: Vec::new(),
                unread_inbox_count: 0,
                ack_required_count: 0,
            },
            checkout: SwarmNextActionCheckoutSummary {
                dirty_path_count: 0,
                dirty_paths: Vec::new(),
            },
            compile_health: SwarmNextActionCompileHealthSummary {
                safe_to_launch_rch: Some(true),
                blocker_count: 0,
                blockers: Vec::new(),
                recommended_alternative_work: Vec::new(),
            },
            verification: SwarmNextActionVerificationSummary {
                rch_source_enabled: true,
                remote_only_required: true,
                remote_only_safe: Some(true),
                healthy_worker_count: Some(1),
                active_remote_build_count: Some(0),
                queued_remote_build_count: Some(0),
                slots_available: Some(1),
                queue_head_slots_needed: None,
                active_build_max_age_seconds: None,
                queue_status: Some("ready".to_owned()),
                verifier_evidence: Vec::new(),
            },
            environment: SwarmNextActionEnvironmentSummary {
                cargo_target_externalized: true,
                tmpdir_externalized: true,
                external_agent_space_present: true,
                disk_pressure_hint_count: 0,
            },
            degraded: Vec::new(),
        }
    }

    fn verification_for_queue_posture(
        remote_only_safe: Option<bool>,
        active_count: Option<u64>,
        queued_count: Option<u64>,
        slots_available: Option<u64>,
        queue_head_slots_needed: Option<u64>,
        active_build_max_age_seconds: Option<u64>,
        queue_status: Option<&str>,
    ) -> SwarmNextActionVerificationSummary {
        SwarmNextActionVerificationSummary {
            rch_source_enabled: remote_only_safe.is_some()
                || active_count.is_some()
                || queued_count.is_some(),
            remote_only_required: true,
            remote_only_safe,
            healthy_worker_count: Some(1),
            active_remote_build_count: active_count,
            queued_remote_build_count: queued_count,
            slots_available,
            queue_head_slots_needed,
            active_build_max_age_seconds,
            queue_status: queue_status.map(str::to_owned),
            verifier_evidence: Vec::new(),
        }
    }

    fn admission_aggression_rank(action: &str) -> u8 {
        match action {
            "queue" => 3,
            "wait" => 2,
            "static_work" => 1,
            "coordinate" => 0,
            other => panic!("unknown admission action {other}"),
        }
    }

    fn service_records(durations_ms: &[u64]) -> Vec<SwarmNextActionServiceTimeEvidenceRecord> {
        durations_ms
            .iter()
            .map(|duration_ms| SwarmNextActionServiceTimeEvidenceRecord {
                command_family: "cargo_test".to_owned(),
                duration_ms: *duration_ms,
                queue_wait_ms: 10_000,
                observed_age_seconds: 60,
                failure_class: None,
                worker_class: Some("linux_x86_64".to_owned()),
                duplicate_bead_attribution: false,
            })
            .collect()
    }

    fn candidate(
        id: &str,
        title: &str,
        source: &'static str,
        priority: Option<i64>,
    ) -> SwarmNextActionCandidate {
        SwarmNextActionCandidate {
            id: id.to_owned(),
            title: title.to_owned(),
            source,
            score_milli: None,
            status: "open".to_owned(),
            priority,
            issue_type: None,
            assignee: None,
            blocked_by: Vec::new(),
            blocked_by_compile_health: false,
            action_hint: "reserve_files_and_start_smallest_useful_slice".to_owned(),
        }
    }

    fn bead(id: &str, title: &str, priority: i64) -> SwarmBriefBead {
        SwarmBriefBead {
            id: id.to_owned(),
            title: title.to_owned(),
            status: "open".to_owned(),
            priority: Some(priority),
            issue_type: None,
            assignee: None,
            created_at: None,
            updated_at: None,
            latest_comment_at: None,
            comment_count: 0,
            source_bucket: "ready".to_owned(),
        }
    }

    fn degradation(
        source: SwarmBriefSourceKind,
        code: &str,
        message: &str,
        repair: Option<String>,
    ) -> SwarmBriefDegradation {
        SwarmBriefDegradation {
            code: code.to_owned(),
            source,
            severity: "warning",
            message: message.to_owned(),
            repair,
        }
    }
}
