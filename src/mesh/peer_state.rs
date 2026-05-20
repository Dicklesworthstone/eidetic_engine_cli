//! SRR6.46.13 peer drift grace state machine.
//!
//! The mesh drift detector should not classify a peer as removable after one
//! failed hello. This module tracks per-peer probe history and separates
//! transient reachability problems from hard stale drift. It is pure policy
//! logic; storage adapters can persist [`MeshPeerStateRow`] in
//! `ee_mesh_peer_state`.

use serde::{Deserialize, Serialize};

use crate::config::EnvVar;

pub const MESH_PEER_STATE_SCHEMA_V1: &str = "ee.mesh.peer_state.v1";
pub const DRIFT_GRACE_SOFT_STALE_PEER_COUNT_HIGH_CODE: &str =
    "drift_grace_soft_stale_peer_count_high";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshDriftPeerState {
    Active,
    SoftStale,
    HardStale,
    Denylisted,
}

impl MeshDriftPeerState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::SoftStale => "soft_stale",
            Self::HardStale => "hard_stale",
            Self::Denylisted => "denylisted",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshDriftThresholds {
    pub soft_stale_after_missed_probes: u32,
    pub soft_stale_after_seconds: u64,
    pub hard_stale_after_missed_probes: u32,
    pub hard_stale_after_seconds: u64,
}

impl Default for MeshDriftThresholds {
    fn default() -> Self {
        Self {
            soft_stale_after_missed_probes: 1,
            soft_stale_after_seconds: 300,
            hard_stale_after_missed_probes: 3,
            hard_stale_after_seconds: 3_600,
        }
    }
}

impl MeshDriftThresholds {
    #[must_use]
    pub fn from_env_values(mut read: impl FnMut(EnvVar) -> Option<String>) -> Self {
        let defaults = Self::default();
        Self {
            soft_stale_after_missed_probes: parse_u32(
                read(EnvVar::MeshDriftSoftStaleAfter),
                defaults.soft_stale_after_missed_probes,
            ),
            soft_stale_after_seconds: parse_u64(
                read(EnvVar::MeshDriftSoftStaleAfterSeconds),
                defaults.soft_stale_after_seconds,
            ),
            hard_stale_after_missed_probes: parse_u32(
                read(EnvVar::MeshDriftHardStaleAfter),
                defaults.hard_stale_after_missed_probes,
            ),
            hard_stale_after_seconds: parse_u64(
                read(EnvVar::MeshDriftHardStaleAfterSeconds),
                defaults.hard_stale_after_seconds,
            ),
        }
    }
}

fn parse_u32(raw: Option<String>, default: u32) -> u32 {
    raw.and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn parse_u64(raw: Option<String>, default: u64) -> u64 {
    raw.and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshPeerProbeOutcome {
    Success,
    MissedHello,
    Denylisted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerStateRow {
    pub schema: String,
    pub peer_node_key: String,
    pub peer_group_id: String,
    pub consecutive_missed_probes: u32,
    pub last_successful_probe_at_epoch_seconds: Option<u64>,
    pub first_observed_at_epoch_seconds: u64,
    pub state: MeshDriftPeerState,
}

impl MeshPeerStateRow {
    #[must_use]
    pub fn active(
        peer_node_key: impl Into<String>,
        peer_group_id: impl Into<String>,
        now_epoch_seconds: u64,
    ) -> Self {
        Self {
            schema: MESH_PEER_STATE_SCHEMA_V1.to_owned(),
            peer_node_key: peer_node_key.into(),
            peer_group_id: peer_group_id.into(),
            consecutive_missed_probes: 0,
            last_successful_probe_at_epoch_seconds: Some(now_epoch_seconds),
            first_observed_at_epoch_seconds: now_epoch_seconds,
            state: MeshDriftPeerState::Active,
        }
    }

    #[must_use]
    pub fn denylisted(mut self) -> Self {
        self.state = MeshDriftPeerState::Denylisted;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPeerProbeObservation {
    pub peer_node_key: String,
    pub peer_group_id: String,
    pub now_epoch_seconds: u64,
    pub outcome: MeshPeerProbeOutcome,
}

impl MeshPeerProbeObservation {
    #[must_use]
    pub fn new(
        peer_node_key: impl Into<String>,
        peer_group_id: impl Into<String>,
        now_epoch_seconds: u64,
        outcome: MeshPeerProbeOutcome,
    ) -> Self {
        Self {
            peer_node_key: peer_node_key.into(),
            peer_group_id: peer_group_id.into(),
            now_epoch_seconds,
            outcome,
        }
    }
}

#[must_use]
pub fn apply_probe_observation(
    existing: Option<&MeshPeerStateRow>,
    observation: &MeshPeerProbeObservation,
    thresholds: MeshDriftThresholds,
) -> MeshPeerStateRow {
    let mut row = existing.cloned().unwrap_or_else(|| MeshPeerStateRow {
        schema: MESH_PEER_STATE_SCHEMA_V1.to_owned(),
        peer_node_key: observation.peer_node_key.clone(),
        peer_group_id: observation.peer_group_id.clone(),
        consecutive_missed_probes: 0,
        last_successful_probe_at_epoch_seconds: None,
        first_observed_at_epoch_seconds: observation.now_epoch_seconds,
        state: MeshDriftPeerState::Active,
    });
    row.peer_group_id = observation.peer_group_id.clone();

    match observation.outcome {
        MeshPeerProbeOutcome::Denylisted => {
            row.state = MeshDriftPeerState::Denylisted;
            row
        }
        MeshPeerProbeOutcome::Success => {
            row.consecutive_missed_probes = 0;
            row.last_successful_probe_at_epoch_seconds = Some(observation.now_epoch_seconds);
            row.state = MeshDriftPeerState::Active;
            row
        }
        MeshPeerProbeOutcome::MissedHello if row.state == MeshDriftPeerState::Denylisted => row,
        MeshPeerProbeOutcome::MissedHello => {
            row.consecutive_missed_probes = row.consecutive_missed_probes.saturating_add(1);
            let last_success = row
                .last_successful_probe_at_epoch_seconds
                .unwrap_or(row.first_observed_at_epoch_seconds);
            let elapsed = observation.now_epoch_seconds.saturating_sub(last_success);
            row.state = classify_missed_probe(row.consecutive_missed_probes, elapsed, thresholds);
            row
        }
    }
}

fn classify_missed_probe(
    missed: u32,
    elapsed_seconds: u64,
    thresholds: MeshDriftThresholds,
) -> MeshDriftPeerState {
    if missed >= thresholds.hard_stale_after_missed_probes
        && elapsed_seconds >= thresholds.hard_stale_after_seconds
    {
        MeshDriftPeerState::HardStale
    } else if missed >= thresholds.soft_stale_after_missed_probes
        && elapsed_seconds >= thresholds.soft_stale_after_seconds
    {
        MeshDriftPeerState::SoftStale
    } else {
        MeshDriftPeerState::Active
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftPeerRef {
    pub peer_node_key: String,
    pub peer_group_id: String,
    pub reason: &'static str,
    pub severity: &'static str,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshDriftGraceReport {
    pub transient_unreachable: Vec<DriftPeerRef>,
    pub stale_peers_in_config: Vec<DriftPeerRef>,
    pub degraded: Vec<MeshDriftGraceDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshDriftGraceDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: &'static str,
}

#[must_use]
pub fn drift_grace_report(rows: &[MeshPeerStateRow]) -> MeshDriftGraceReport {
    let mut report = MeshDriftGraceReport::default();
    for row in rows {
        match row.state {
            MeshDriftPeerState::Active | MeshDriftPeerState::Denylisted => {}
            MeshDriftPeerState::SoftStale => report.transient_unreachable.push(DriftPeerRef {
                peer_node_key: row.peer_node_key.clone(),
                peer_group_id: row.peer_group_id.clone(),
                reason: "grace_period_soft_stale",
                severity: "info",
            }),
            MeshDriftPeerState::HardStale => report.stale_peers_in_config.push(DriftPeerRef {
                peer_node_key: row.peer_node_key.clone(),
                peer_group_id: row.peer_group_id.clone(),
                reason: "consecutive_probes_missed",
                severity: "warning",
            }),
        }
    }
    report.transient_unreachable.sort_by(|left, right| {
        left.peer_node_key
            .cmp(&right.peer_node_key)
            .then_with(|| left.peer_group_id.cmp(&right.peer_group_id))
    });
    report.stale_peers_in_config.sort_by(|left, right| {
        left.peer_node_key
            .cmp(&right.peer_node_key)
            .then_with(|| left.peer_group_id.cmp(&right.peer_group_id))
    });

    let materialized = rows
        .iter()
        .filter(|row| row.state != MeshDriftPeerState::Denylisted)
        .count();
    let soft = report.transient_unreachable.len();
    if materialized > 0 && soft.saturating_mul(4) > materialized {
        report.degraded.push(MeshDriftGraceDegradation {
            code: DRIFT_GRACE_SOFT_STALE_PEER_COUNT_HIGH_CODE,
            severity: "warning",
            message: format!(
                "{soft} of {materialized} materialized mesh peers are soft-stale; the tailnet may be partitioning."
            ),
            repair: "Investigate Tailscale connectivity before running auto-enroll repair.",
        });
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EnvVar;

    fn miss_at(existing: &MeshPeerStateRow, now: u64) -> MeshPeerStateRow {
        apply_probe_observation(
            Some(existing),
            &MeshPeerProbeObservation::new(
                existing.peer_node_key.clone(),
                existing.peer_group_id.clone(),
                now,
                MeshPeerProbeOutcome::MissedHello,
            ),
            MeshDriftThresholds::default(),
        )
    }

    #[test]
    fn drift_state_machine_transitions_active_to_soft_stale_after_default_threshold() {
        let active = MeshPeerStateRow::active("nodekey:one", "pg_alpha", 100);
        let stale = miss_at(&active, 401);

        assert_eq!(stale.consecutive_missed_probes, 1);
        assert_eq!(stale.state, MeshDriftPeerState::SoftStale);
    }

    #[test]
    fn drift_state_machine_transitions_soft_stale_back_to_active_on_successful_probe() {
        let active = MeshPeerStateRow::active("nodekey:one", "pg_alpha", 100);
        let soft = miss_at(&active, 401);
        let recovered = apply_probe_observation(
            Some(&soft),
            &MeshPeerProbeObservation::new(
                "nodekey:one",
                "pg_alpha",
                402,
                MeshPeerProbeOutcome::Success,
            ),
            MeshDriftThresholds::default(),
        );

        assert_eq!(recovered.state, MeshDriftPeerState::Active);
        assert_eq!(recovered.consecutive_missed_probes, 0);
        assert_eq!(recovered.last_successful_probe_at_epoch_seconds, Some(402));
    }

    #[test]
    fn drift_state_machine_transitions_to_hard_stale_after_three_consecutive_misses_and_one_hour() {
        let active = MeshPeerStateRow::active("nodekey:one", "pg_alpha", 100);
        let one = miss_at(&active, 401);
        let two = miss_at(&one, 1_000);
        let three = miss_at(&two, 3_701);

        assert_eq!(three.consecutive_missed_probes, 3);
        assert_eq!(three.state, MeshDriftPeerState::HardStale);
    }

    #[test]
    fn drift_state_machine_excludes_soft_stale_peers_from_stale_peers_in_config_report() {
        let active = MeshPeerStateRow::active("nodekey:one", "pg_alpha", 100);
        let soft = miss_at(&active, 401);
        let report = drift_grace_report(&[soft]);

        assert_eq!(report.transient_unreachable.len(), 1);
        assert!(report.stale_peers_in_config.is_empty());
        assert_eq!(report.transient_unreachable[0].severity, "info");
    }

    #[test]
    fn drift_state_machine_includes_hard_stale_peers_in_stale_peers_in_config_report() {
        let active = MeshPeerStateRow::active("nodekey:one", "pg_alpha", 100);
        let hard = miss_at(&miss_at(&miss_at(&active, 401), 1_000), 3_701);
        let report = drift_grace_report(&[hard]);

        assert!(report.transient_unreachable.is_empty());
        assert_eq!(report.stale_peers_in_config.len(), 1);
        assert_eq!(
            report.stale_peers_in_config[0].reason,
            "consecutive_probes_missed"
        );
    }

    #[test]
    fn drift_state_machine_denylisted_peer_not_reported_in_either_drift_list() {
        let row = MeshPeerStateRow::active("nodekey:one", "pg_alpha", 100).denylisted();
        let report = drift_grace_report(&[row]);

        assert!(report.transient_unreachable.is_empty());
        assert!(report.stale_peers_in_config.is_empty());
        assert!(report.degraded.is_empty());
    }

    #[test]
    fn drift_state_machine_respects_env_var_overrides_for_thresholds() {
        let thresholds = MeshDriftThresholds::from_env_values(|var| match var {
            EnvVar::MeshDriftSoftStaleAfter => Some("2".to_owned()),
            EnvVar::MeshDriftSoftStaleAfterSeconds => Some("10".to_owned()),
            EnvVar::MeshDriftHardStaleAfter => Some("4".to_owned()),
            EnvVar::MeshDriftHardStaleAfterSeconds => Some("20".to_owned()),
            _ => None,
        });
        let active = MeshPeerStateRow::active("nodekey:one", "pg_alpha", 100);
        let one = apply_probe_observation(
            Some(&active),
            &MeshPeerProbeObservation::new(
                "nodekey:one",
                "pg_alpha",
                111,
                MeshPeerProbeOutcome::MissedHello,
            ),
            thresholds,
        );
        let two = apply_probe_observation(
            Some(&one),
            &MeshPeerProbeObservation::new(
                "nodekey:one",
                "pg_alpha",
                112,
                MeshPeerProbeOutcome::MissedHello,
            ),
            thresholds,
        );

        assert_eq!(one.state, MeshDriftPeerState::Active);
        assert_eq!(two.state, MeshDriftPeerState::SoftStale);
    }

    #[test]
    fn drift_state_machine_soft_stale_count_high_warns_when_over_quarter_of_materialized_peers() {
        let soft = miss_at(&MeshPeerStateRow::active("nodekey:a", "pg_alpha", 100), 401);
        let active = MeshPeerStateRow::active("nodekey:b", "pg_alpha", 401);
        let report = drift_grace_report(&[soft, active]);

        assert_eq!(report.degraded.len(), 1);
        assert_eq!(
            report.degraded[0].code,
            "drift_grace_soft_stale_peer_count_high"
        );
    }
}
