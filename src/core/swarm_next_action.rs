//! Read-only next-action snapshot for swarm work allocation.
//!
//! This module intentionally builds on the existing `swarm brief` collectors.
//! SWA1 defines the stable input snapshot; later SWA beads can add ranking and
//! reservation suggestions without re-collecting source state.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::core::swarm_brief::{
    SwarmBriefCollectOptions, SwarmBriefCommandRunner, SwarmBriefDegradation,
    SwarmBriefFileReservation, SwarmBriefReport, SwarmBriefSourceKind, collect_swarm_brief,
};

pub const SWARM_NEXT_ACTION_SCHEMA_V1: &str = "ee.swarm_next_action.v1";
pub const SWARM_NEXT_ACTION_REDACTION_STATUS: &str =
    "counts_ids_statuses_paths_redacted_no_mail_body_no_file_content";
const EXTERNAL_AGENT_SPACE_ROOT: &str = "/Volumes/USBNVME16TB/temp_agent_space";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmNextActionVerificationSummary {
    pub rch_source_enabled: bool,
    pub remote_only_required: bool,
    pub remote_only_safe: Option<bool>,
    pub healthy_worker_count: Option<u64>,
    pub active_remote_build_count: Option<u64>,
    pub queued_remote_build_count: Option<u64>,
    pub slots_available: Option<u64>,
    pub queue_head_slots_needed: Option<u64>,
    pub queue_status: Option<String>,
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
        RchCodexHookCapability, RchLocalCapabilityReport, RchQueueHealth, RchWorkerProbeSummary,
        SwarmBriefBead, SwarmBriefBvPick, SwarmBriefBvSummary, SwarmBriefDegradation,
        SwarmBriefDirtyFile, SwarmBriefFileReservation, SwarmBriefInboxSummary,
        SwarmBriefSourceKind,
    };

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
                status: "saturated".to_owned(),
            }),
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
            snapshot.verification.queue_status.as_deref(),
            Some("saturated")
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
    fn wildcard_path_matching_covers_exact_glob_and_question_patterns() {
        assert!(path_matches_pattern("src/db/mod.rs", "src/db/mod.rs"));
        assert!(path_matches_pattern("src/db/mod.rs", "src/db/*.rs"));
        assert!(path_matches_pattern("src/db/a.rs", "src/db/?.rs"));
        assert!(!path_matches_pattern("src/core/status.rs", "src/db/*.rs"));
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
