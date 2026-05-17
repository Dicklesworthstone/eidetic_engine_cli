//! Read-only next-action snapshot for swarm work allocation.
//!
//! This module intentionally builds on the existing `swarm brief` collectors.
//! SWA1 defines the stable input snapshot; later SWA beads can add ranking and
//! reservation suggestions without re-collecting source state.

use std::env;
use std::path::Path;

use serde::Serialize;

use crate::core::swarm_brief::{
    SwarmBriefCollectOptions, SwarmBriefCommandRunner, SwarmBriefDegradation, SwarmBriefReport,
    SwarmBriefSourceKind, collect_swarm_brief,
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
pub struct SwarmNextActionVerificationSummary {
    pub rch_source_enabled: bool,
    pub remote_only_required: bool,
    pub remote_only_safe: Option<bool>,
    pub healthy_worker_count: Option<u64>,
    pub active_remote_build_count: Option<u64>,
    pub queued_remote_build_count: Option<u64>,
    pub slots_available: Option<u64>,
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
    let brief = collect_swarm_brief(options, runner);
    SwarmNextActionSnapshot::from_swarm_brief(&brief)
}

impl SwarmNextActionSnapshot {
    #[must_use]
    pub fn from_swarm_brief(brief: &SwarmBriefReport) -> Self {
        let mut candidates = candidates_from_brief(brief);
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
            verification: verification_summary(brief),
            environment: environment_summary(brief),
            degraded,
        }
    }
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

fn candidates_from_brief(brief: &SwarmBriefReport) -> Vec<SwarmNextActionCandidate> {
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
                blocked_by: Vec::new(),
                action_hint: "inspect_and_reserve_before_editing".to_owned(),
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
        SwarmBriefBead, SwarmBriefBvPick, SwarmBriefBvSummary, SwarmBriefFileReservation,
        SwarmBriefInboxSummary,
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
}
