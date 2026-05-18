//! Audit-lane public contract constants.
//!
//! The runtime queue and writer land in later `bd-wp5ac` slices. This module
//! anchors the schema and degraded-code strings so docs, fixtures, and tests can
//! refer to one source-owned vocabulary before call sites start emitting events.

pub const AUDIT_LANE_SCHEMA_V1: &str = "ee.audit_lane.v1";
pub const AUDIT_LANE_SOURCE_LABEL: &str = "audit_lane";

pub const AUDIT_BACKPRESSURE_CODE: &str = "audit_backpressure";
pub const AUDIT_LANE_SHUTDOWN_DRAIN_TIMEOUT_CODE: &str = "audit_lane_shutdown_drain_timeout";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditLanePhase {
    Enqueue,
    Drain,
    BatchCommit,
    Shutdown,
    Backpressure,
}

impl AuditLanePhase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enqueue => "enqueue",
            Self::Drain => "drain",
            Self::BatchCommit => "batch_commit",
            Self::Shutdown => "shutdown",
            Self::Backpressure => "backpressure",
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Enqueue,
            Self::Drain,
            Self::BatchCommit,
            Self::Shutdown,
            Self::Backpressure,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_lane_phase_order_is_schema_order() {
        let phases: Vec<&str> = AuditLanePhase::all()
            .iter()
            .map(|phase| phase.as_str())
            .collect();
        assert_eq!(
            phases,
            vec![
                "enqueue",
                "drain",
                "batch_commit",
                "shutdown",
                "backpressure"
            ]
        );
    }

    #[test]
    fn audit_lane_degraded_codes_are_snake_case() {
        for code in [
            AUDIT_BACKPRESSURE_CODE,
            AUDIT_LANE_SHUTDOWN_DRAIN_TIMEOUT_CODE,
        ] {
            assert!(
                code.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
                "{code} must stay fixture-compatible snake_case"
            );
        }
    }
}
