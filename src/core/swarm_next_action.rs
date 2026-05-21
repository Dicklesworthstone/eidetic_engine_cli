//! Read-only next-action snapshot for swarm work allocation.
//!
//! This module intentionally builds on the existing `swarm brief` collectors.
//! SWA1 defines the stable input snapshot; later SWA beads can add ranking and
//! reservation suggestions without re-collecting source state.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::Path;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::core::swarm_brief::{
    SwarmBriefCollectOptions, SwarmBriefCommandRunner, SwarmBriefDegradation,
    SwarmBriefFileReservation, SwarmBriefReport, SwarmBriefSourceKind, collect_swarm_brief,
};

pub const SWARM_NEXT_ACTION_SCHEMA_V1: &str = "ee.swarm_next_action.v1";
pub const SWARM_NEXT_ACTION_REDACTION_STATUS: &str =
    "counts_ids_statuses_paths_redacted_no_mail_body_no_file_content";
const EXTERNAL_AGENT_SPACE_ROOT: &str = "/Volumes/USBNVME16TB/temp_agent_space";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmNextActionSnapshot {
    pub schema: &'static str,
    pub workspace: String,
    pub redaction_status: &'static str,
    pub inputs: SwarmNextActionInputSummary,
    pub candidates: Vec<SwarmNextActionCandidate>,
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
        let mut state = serializer.serialize_struct("SwarmNextActionSnapshot", 12)?;
        state.serialize_field("schema", &self.schema)?;
        state.serialize_field("workspace", &self.workspace)?;
        state.serialize_field("redactionStatus", &self.redaction_status)?;
        state.serialize_field("inputs", &self.inputs)?;
        state.serialize_field("candidates", &self.candidates)?;
        state.serialize_field("recommendationCards", &self.recommendation_cards())?;
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
    pub assignee: Option<String>,
    pub blocked_by: Vec<String>,
    pub blocked_by_compile_health: bool,
    pub action_hint: String,
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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionAdmissionCertificate {
    pub schema: &'static str,
    pub action: &'static str,
    pub rule_id: &'static str,
    pub confidence: &'static str,
    pub service_class: &'static str,
    pub evidence: Vec<String>,
    pub assumptions: Vec<&'static str>,
    pub proof_obligations: Vec<&'static str>,
    pub safety_invariant: &'static str,
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
            evidence: self.queue_evidence(),
            assumptions,
            proof_obligations,
            safety_invariant: "monotonic_queue_pressure_never_increases_aggression",
        }
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
            coordination: coordination_summary(brief),
            checkout: SwarmNextActionCheckoutSummary {
                dirty_path_count: dirty_paths.len(),
                dirty_paths,
            },
            compile_health,
            verification: verification_summary(brief),
            environment: environment_summary(brief),
            degraded,
        }
    }

    #[must_use]
    pub fn recommendation_cards(&self) -> Vec<SwarmNextActionRecommendationCard> {
        recommendation_cards_from_snapshot(self)
    }
}

#[must_use]
pub fn verifier_evidence_from_json(value: &Value) -> Vec<SwarmNextActionRecentFirstError> {
    let mut evidence = Vec::new();
    collect_verifier_evidence_items(value, &mut evidence);
    evidence.sort();
    evidence.dedup();
    evidence
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
    let first = value.get("first_error").or_else(|| value.get("firstError"));
    let file = value
        .get("first_error_file")
        .or_else(|| value.get("firstErrorFile"))
        .and_then(Value::as_str)
        .or_else(|| {
            first
                .and_then(Value::as_object)
                .and_then(|object| object.get("file").or_else(|| object.get("path")))
                .and_then(Value::as_str)
        })
        .map(normalize_remote_repo_path)?;
    let degraded_codes = string_array(
        value
            .get("degraded_codes")
            .or_else(|| value.get("degradedCodes")),
    );
    let status = string_value(value.get("status").or_else(|| value.get("result")));
    let failure_like = status
        .as_deref()
        .is_some_and(|status| matches!(status, "remote_failure" | "failed" | "failure"))
        || degraded_codes
            .iter()
            .any(|code| code == "rch_verify_remote_command_failed");
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
    Some(SwarmNextActionRecentFirstError {
        file,
        line,
        command_kind: string_value(
            value
                .get("command_kind")
                .or_else(|| value.get("commandKind")),
        ),
        command: string_value(
            value
                .get("command_text")
                .or_else(|| value.get("commandText"))
                .or_else(|| value.get("command")),
        ),
        command_hash: string_value(
            value
                .get("command_hash")
                .or_else(|| value.get("commandHash")),
        ),
        status,
        degraded_codes,
    })
}

fn string_value(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    let mut strings = items
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    strings.sort();
    strings.dedup();
    strings
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

fn recommendation_cards_from_snapshot(
    snapshot: &SwarmNextActionSnapshot,
) -> Vec<SwarmNextActionRecommendationCard> {
    if snapshot.candidates.is_empty() {
        return no_action_recommendation_cards(snapshot);
    }

    let mut candidate_counts = BTreeMap::<&str, usize>::new();
    for candidate in &snapshot.candidates {
        *candidate_counts.entry(candidate.id.as_str()).or_default() += 1;
    }

    let caveats = recommendation_evidence_caveats(snapshot);
    let has_compile_owner_blocker = snapshot
        .compile_health
        .blockers
        .iter()
        .any(|blocker| blocker.owner_agent.is_some());
    let mut cards = snapshot
        .candidates
        .iter()
        .map(|candidate| {
            recommendation_card_for_candidate(
                candidate,
                candidate_counts
                    .get(candidate.id.as_str())
                    .copied()
                    .unwrap_or(1),
                &caveats,
                has_compile_owner_blocker,
            )
        })
        .collect::<Vec<_>>();
    cards.sort();
    cards.dedup();
    cards
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
    candidate_id_count: usize,
    evidence_caveats: &[String],
    has_compile_owner_blocker: bool,
) -> SwarmNextActionRecommendationCard {
    let duplicate = candidate_id_count > 1;
    let blocked_by_owner = candidate.assignee.is_some()
        || (candidate.blocked_by_compile_health && has_compile_owner_blocker);
    let decision = if duplicate {
        "duplicate_rejected"
    } else if blocked_by_owner {
        "blocked_by_owner"
    } else if candidate.status == "unknown" {
        "new_bead_recommended"
    } else {
        "refine_existing_bead"
    };
    let fallback_decision = match decision {
        "duplicate_rejected" => Some("refine_existing_bead"),
        "blocked_by_owner" => Some("message_owner_before_editing"),
        _ => None,
    };

    SwarmNextActionRecommendationCard {
        card_id: format!("{decision}:{}", candidate.id),
        candidate_id: Some(candidate.id.clone()),
        candidate_source: candidate.source,
        candidate_summary: candidate.title.clone(),
        decision,
        confidence: recommendation_confidence(candidate, decision, evidence_caveats),
        score_inputs: recommendation_score_inputs(candidate),
        overlap: overlap_decision_for_candidate(candidate, decision, duplicate),
        proof_obligations: recommendation_proof_obligations(candidate, decision),
        evidence_caveats: evidence_caveats.to_vec(),
        fallback_decision,
    }
}

fn overlap_decision_for_candidate(
    candidate: &SwarmNextActionCandidate,
    decision: &'static str,
    duplicate: bool,
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
        rejected_duplicate_reason: if duplicate {
            Some("candidate_id_already_present")
        } else {
            None
        },
        selected_relation: match decision {
            "new_bead_recommended" => "new_child",
            "duplicate_rejected" | "refine_existing_bead" => "existing_bead",
            "blocked_by_owner" => "owner_coordination_required",
            _ => "none",
        },
    }
}

fn recommendation_score_inputs(
    candidate: &SwarmNextActionCandidate,
) -> Vec<SwarmNextActionScoreInput> {
    let mut inputs = vec![
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
    obligations.into_iter().collect()
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
        "duplicate_rejected" | "blocked_by_owner" | "no_action_recommended"
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

fn verification_summary(brief: &SwarmBriefReport) -> SwarmNextActionVerificationSummary {
    let rch = brief.rch_local_capability.as_ref();
    SwarmNextActionVerificationSummary {
        rch_source_enabled: rch.is_some()
            || brief
                .sources
                .iter()
                .any(|source| source.source == SwarmBriefSourceKind::Rch),
        remote_only_required: rch.is_some_and(|report| report.remote_only_required),
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
        SwarmBriefDegradation, SwarmBriefDirtyFile, SwarmBriefFileReservation,
        SwarmBriefInboxSummary, SwarmBriefSourceKind,
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

        let json = serde_json::to_value(&snapshot).expect("snapshot serializes");
        assert_eq!(
            json.get("recommendationCards")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
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
    fn wildcard_path_matching_covers_exact_glob_and_question_patterns() {
        assert!(path_matches_pattern("src/db/mod.rs", "src/db/mod.rs"));
        assert!(path_matches_pattern("src/db/mod.rs", "src/db/*.rs"));
        assert!(path_matches_pattern("src/db/a.rs", "src/db/?.rs"));
        assert!(!path_matches_pattern("src/core/status.rs", "src/db/*.rs"));
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
            assignee: None,
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
