//! bd-1eq3l.5 - Pure Agent Mail coordination overlay for workspace hygiene.
//!
//! This module consumes pre-collected Agent Mail facts and classifier rows. It
//! does not query Agent Mail, read message bodies, release reservations, or
//! mutate state. Callers own collection and timeout handling.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::core::hygiene_classifier::ClassificationRow;
use crate::models::degradation::WORKSPACE_HYGIENE_AGENT_MAIL_UNAVAILABLE_CODE;

pub const HYGIENE_COORDINATION_OVERLAY_SCHEMA_V1: &str = "ee.hygiene_coordination_overlay.v1";

pub mod reason {
    pub const ACTIVE_EXCLUSIVE_RESERVATION: &str = "active_exclusive_reservation";
    pub const SHARED_RESERVATION_OBSERVED: &str = "shared_reservation_observed";
    pub const EXPIRED_RESERVATION_IGNORED: &str = "expired_reservation_ignored";
    pub const SELF_RESERVATION_IGNORED: &str = "self_reservation_ignored";
    pub const AGENT_MAIL_UNAVAILABLE: &str = "agent_mail_unavailable";
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentMailCoordinationInput {
    Unavailable,
    Available {
        reservations: Vec<AgentMailReservation>,
        active_agents: Vec<ActiveAgent>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMailReservation {
    pub path_pattern: String,
    pub holder_agent: String,
    pub exclusive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reservation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bead_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveAgent {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HygieneCoordinationOverlay {
    pub schema: &'static str,
    pub agent_mail_available: bool,
    pub active_agent_count: usize,
    pub reservation_count: usize,
    pub blocked_by_coordination: Vec<CoordinationBlockedPath>,
    pub observed_shared_reservations: Vec<ReservationObservation>,
    pub ignored_reservations: Vec<ReservationObservation>,
    pub degraded_codes: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinationBlockedPath {
    pub path: String,
    pub holder_agent: String,
    pub path_pattern: String,
    pub exclusive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reservation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bead_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub reasons: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReservationObservation {
    pub path: String,
    pub holder_agent: String,
    pub path_pattern: String,
    pub exclusive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reservation_id: Option<String>,
    pub reasons: Vec<&'static str>,
}

/// Apply a pre-collected Agent Mail snapshot to dirty-path classifier rows.
#[must_use]
pub fn overlay_coordination_state(
    rows: &[ClassificationRow],
    input: &AgentMailCoordinationInput,
    now: DateTime<Utc>,
    self_agent_name: Option<&str>,
) -> HygieneCoordinationOverlay {
    match input {
        AgentMailCoordinationInput::Unavailable => HygieneCoordinationOverlay {
            schema: HYGIENE_COORDINATION_OVERLAY_SCHEMA_V1,
            agent_mail_available: false,
            active_agent_count: 0,
            reservation_count: 0,
            blocked_by_coordination: Vec::new(),
            observed_shared_reservations: Vec::new(),
            ignored_reservations: Vec::new(),
            degraded_codes: vec![WORKSPACE_HYGIENE_AGENT_MAIL_UNAVAILABLE_CODE],
        },
        AgentMailCoordinationInput::Available {
            reservations,
            active_agents,
        } => {
            let mut blocked_by_coordination = Vec::new();
            let mut observed_shared_reservations = Vec::new();
            let mut ignored_reservations = Vec::new();

            for row in rows {
                for reservation in reservations {
                    if !path_matches_pattern(&row.path, &reservation.path_pattern) {
                        continue;
                    }
                    if reservation_is_expired(reservation, now) {
                        ignored_reservations.push(observation(
                            &row.path,
                            reservation,
                            vec![reason::EXPIRED_RESERVATION_IGNORED],
                        ));
                        continue;
                    }
                    if Some(reservation.holder_agent.as_str()) == self_agent_name {
                        ignored_reservations.push(observation(
                            &row.path,
                            reservation,
                            vec![reason::SELF_RESERVATION_IGNORED],
                        ));
                        continue;
                    }
                    if reservation.exclusive {
                        blocked_by_coordination.push(blocked_path(row, reservation));
                    } else {
                        observed_shared_reservations.push(observation(
                            &row.path,
                            reservation,
                            vec![reason::SHARED_RESERVATION_OBSERVED],
                        ));
                    }
                }
            }

            blocked_by_coordination.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| left.holder_agent.cmp(&right.holder_agent))
                    .then_with(|| left.path_pattern.cmp(&right.path_pattern))
                    .then_with(|| left.reservation_id.cmp(&right.reservation_id))
            });
            observed_shared_reservations.sort_by(compare_observations);
            ignored_reservations.sort_by(compare_observations);

            HygieneCoordinationOverlay {
                schema: HYGIENE_COORDINATION_OVERLAY_SCHEMA_V1,
                agent_mail_available: true,
                active_agent_count: active_agents.len(),
                reservation_count: reservations.len(),
                blocked_by_coordination,
                observed_shared_reservations,
                ignored_reservations,
                degraded_codes: Vec::new(),
            }
        }
    }
}

fn blocked_path(
    row: &ClassificationRow,
    reservation: &AgentMailReservation,
) -> CoordinationBlockedPath {
    CoordinationBlockedPath {
        path: row.path.clone(),
        holder_agent: reservation.holder_agent.clone(),
        path_pattern: reservation.path_pattern.clone(),
        exclusive: reservation.exclusive,
        expires_at: reservation.expires_at.clone(),
        reservation_id: reservation.reservation_id.clone(),
        bead_id: reservation.bead_id.clone(),
        thread_id: reservation.thread_id.clone(),
        reasons: vec![reason::ACTIVE_EXCLUSIVE_RESERVATION],
    }
}

fn observation(
    path: &str,
    reservation: &AgentMailReservation,
    reasons: Vec<&'static str>,
) -> ReservationObservation {
    ReservationObservation {
        path: path.to_owned(),
        holder_agent: reservation.holder_agent.clone(),
        path_pattern: reservation.path_pattern.clone(),
        exclusive: reservation.exclusive,
        expires_at: reservation.expires_at.clone(),
        reservation_id: reservation.reservation_id.clone(),
        reasons,
    }
}

fn compare_observations(
    left: &ReservationObservation,
    right: &ReservationObservation,
) -> std::cmp::Ordering {
    left.path
        .cmp(&right.path)
        .then_with(|| left.holder_agent.cmp(&right.holder_agent))
        .then_with(|| left.path_pattern.cmp(&right.path_pattern))
        .then_with(|| left.reservation_id.cmp(&right.reservation_id))
}

fn reservation_is_expired(reservation: &AgentMailReservation, now: DateTime<Utc>) -> bool {
    let Some(expires_at) = reservation.expires_at.as_deref() else {
        return false;
    };
    let Ok(parsed) = DateTime::parse_from_rfc3339(expires_at) else {
        return false;
    };
    parsed.with_timezone(&Utc) <= now
}

fn path_matches_pattern(path: &str, pattern: &str) -> bool {
    path == pattern || wildcard_match(path.as_bytes(), pattern.as_bytes())
}

fn wildcard_match(path: &[u8], pattern: &[u8]) -> bool {
    let (mut path_index, mut pattern_index) = (0, 0);
    let mut star_index = None;
    let mut star_path_index = 0;

    while path_index < path.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == path[path_index])
        {
            path_index += 1;
            pattern_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_path_index = path_index;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            star_path_index += 1;
            path_index = star_path_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }

    pattern_index == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hygiene_classifier::{
        Bucket, GitState, HYGIENE_CLASSIFICATION_ROW_SCHEMA_V1, Kind,
    };

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-17T05:30:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc)
    }

    fn row(path: &str) -> ClassificationRow {
        ClassificationRow {
            schema: HYGIENE_CLASSIFICATION_ROW_SCHEMA_V1,
            path: path.to_owned(),
            git_state: GitState {
                staged: ".".to_owned(),
                unstaged: "M".to_owned(),
                entry_kind: "ordinary".to_owned(),
                original_path: None,
            },
            bucket: Bucket::StageCandidate,
            kind: Kind::Source,
            confidence: 0.85,
            reasons: vec!["src_rust_source"],
            suggested_group: Some("source".to_owned()),
            redacted_evidence: Vec::new(),
        }
    }

    fn reservation(pattern: &str, holder: &str, exclusive: bool) -> AgentMailReservation {
        AgentMailReservation {
            path_pattern: pattern.to_owned(),
            holder_agent: holder.to_owned(),
            exclusive,
            expires_at: Some("2026-05-17T06:30:00Z".to_owned()),
            reservation_id: Some(format!("res-{holder}")),
            bead_id: Some("bd-test".to_owned()),
            thread_id: Some("bd-test".to_owned()),
        }
    }

    #[test]
    fn unavailable_agent_mail_degrades_without_blocking_paths() {
        let overlay = overlay_coordination_state(
            &[row("src/core/search.rs")],
            &AgentMailCoordinationInput::Unavailable,
            now(),
            Some("GoldenCompass"),
        );
        assert!(!overlay.agent_mail_available);
        assert_eq!(
            overlay.degraded_codes,
            vec![WORKSPACE_HYGIENE_AGENT_MAIL_UNAVAILABLE_CODE]
        );
        assert!(overlay.blocked_by_coordination.is_empty());
    }

    #[test]
    fn available_empty_reservations_reports_no_blocks() {
        let overlay = overlay_coordination_state(
            &[row("src/core/search.rs")],
            &AgentMailCoordinationInput::Available {
                reservations: Vec::new(),
                active_agents: vec![ActiveAgent {
                    name: "GoldenCompass".to_owned(),
                    last_active_at: None,
                }],
            },
            now(),
            Some("GoldenCompass"),
        );
        assert!(overlay.agent_mail_available);
        assert_eq!(overlay.active_agent_count, 1);
        assert_eq!(overlay.reservation_count, 0);
        assert!(overlay.degraded_codes.is_empty());
        assert!(overlay.blocked_by_coordination.is_empty());
    }

    #[test]
    fn overlapping_exclusive_reservation_blocks_dirty_path() {
        let overlay = overlay_coordination_state(
            &[row("src/core/search.rs")],
            &AgentMailCoordinationInput::Available {
                reservations: vec![reservation("src/core/search.rs", "OtherAgent", true)],
                active_agents: Vec::new(),
            },
            now(),
            Some("GoldenCompass"),
        );
        assert_eq!(overlay.blocked_by_coordination.len(), 1);
        let blocked = &overlay.blocked_by_coordination[0];
        assert_eq!(blocked.path, "src/core/search.rs");
        assert_eq!(blocked.holder_agent, "OtherAgent");
        assert!(
            blocked
                .reasons
                .contains(&reason::ACTIVE_EXCLUSIVE_RESERVATION)
        );
    }

    #[test]
    fn glob_reservation_matches_nested_dirty_path() {
        let overlay = overlay_coordination_state(
            &[row("src/core/search.rs")],
            &AgentMailCoordinationInput::Available {
                reservations: vec![reservation("src/core/*.rs", "OtherAgent", true)],
                active_agents: Vec::new(),
            },
            now(),
            Some("GoldenCompass"),
        );
        assert_eq!(overlay.blocked_by_coordination.len(), 1);
    }

    #[test]
    fn non_overlapping_reservation_does_not_block() {
        let overlay = overlay_coordination_state(
            &[row("src/core/search.rs")],
            &AgentMailCoordinationInput::Available {
                reservations: vec![reservation("src/pack/mod.rs", "OtherAgent", true)],
                active_agents: Vec::new(),
            },
            now(),
            Some("GoldenCompass"),
        );
        assert!(overlay.blocked_by_coordination.is_empty());
        assert!(overlay.ignored_reservations.is_empty());
    }

    #[test]
    fn expired_reservation_is_ignored() {
        let mut expired = reservation("src/core/search.rs", "OtherAgent", true);
        expired.expires_at = Some("2026-05-17T04:30:00Z".to_owned());
        let overlay = overlay_coordination_state(
            &[row("src/core/search.rs")],
            &AgentMailCoordinationInput::Available {
                reservations: vec![expired],
                active_agents: Vec::new(),
            },
            now(),
            Some("GoldenCompass"),
        );
        assert!(overlay.blocked_by_coordination.is_empty());
        assert_eq!(overlay.ignored_reservations.len(), 1);
        assert!(
            overlay.ignored_reservations[0]
                .reasons
                .contains(&reason::EXPIRED_RESERVATION_IGNORED)
        );
    }

    #[test]
    fn shared_reservation_is_observed_without_blocking() {
        let overlay = overlay_coordination_state(
            &[row("src/core/search.rs")],
            &AgentMailCoordinationInput::Available {
                reservations: vec![reservation("src/core/search.rs", "OtherAgent", false)],
                active_agents: Vec::new(),
            },
            now(),
            Some("GoldenCompass"),
        );
        assert!(overlay.blocked_by_coordination.is_empty());
        assert_eq!(overlay.observed_shared_reservations.len(), 1);
        assert!(
            overlay.observed_shared_reservations[0]
                .reasons
                .contains(&reason::SHARED_RESERVATION_OBSERVED)
        );
    }

    #[test]
    fn self_reservation_is_ignored_without_blocking() {
        let overlay = overlay_coordination_state(
            &[row("src/core/search.rs")],
            &AgentMailCoordinationInput::Available {
                reservations: vec![reservation("src/core/search.rs", "GoldenCompass", true)],
                active_agents: Vec::new(),
            },
            now(),
            Some("GoldenCompass"),
        );
        assert!(overlay.blocked_by_coordination.is_empty());
        assert_eq!(overlay.ignored_reservations.len(), 1);
        assert!(
            overlay.ignored_reservations[0]
                .reasons
                .contains(&reason::SELF_RESERVATION_IGNORED)
        );
    }
}
