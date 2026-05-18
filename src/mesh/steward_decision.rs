//! SRR6.46.14 — Steward periodic drift reconciliation (pure decision module).
//!
//! Opt-in user-requested steward job that periodically reconciles
//! auto-enrollment with the current tailnet state. Closes the
//! "I added a teammate but my agent never picked them up" UX gap
//! without forcing the user to manually run `ee mesh auto-enroll`
//! whenever the tailnet changes.
//!
//! This module owns the **pure decision logic**:
//!
//! - [`decide_steward_outcome`] takes the resolved facts (enabled flag,
//!   drift severity + drift kind, today's reconciliation count, daily
//!   cap) and returns the outcome (`NoOp`, `Triggered`, `Refused`,
//!   `DailyCapReached`, `NotEnabled`) plus the audit reason string
//!   the caller emits.
//! - [`apply_interval_jitter`] computes the next-fire wall-clock
//!   given the base interval and a ±jitter window. Pure: a caller-
//!   supplied jitter value drives the result, so tests can pin a
//!   deterministic seed.
//!
//! Why opt-in (not default-on): quietly mutating peer-group config on
//! a schedule violates the SRR6.46.5 forensic-audit-row consent model.
//! With opt-in, the user actively requests the convenience and signs
//! up for the audit-trail side effect.
//!
//! The state-file IO (`~/.local/share/ee/steward/auto_enroll_state.json`),
//! the `ee mesh steward [status | run-now]` CLI surface, and the
//! daemon-side scheduler all land in follow-up slices — those touch
//! `src/cli/mod.rs`, `src/steward/mod.rs`, and the filesystem, which
//! are hot-file zones at the time this module lands.

use serde::{Deserialize, Serialize};

/// JSON schema identifier for the `ee mesh steward status --json`
/// surface. Pinned here so the future renderer and the schema-lifecycle
/// drift gate agree on exactly one string.
pub const STEWARD_STATUS_SCHEMA_V1: &str = "ee.mesh.steward.status.v1";

/// Audit event types the steward emits. Held as `&'static str`
/// constants so the audit-row caller cannot drift from the documented
/// SRR6.46.14 vocabulary.
pub mod audit_events {
    pub const RECONCILIATION_SKIPPED: &str = "mesh.steward_reconciliation_skipped";
    pub const RECONCILIATION_TRIGGERED: &str = "mesh.steward_reconciliation_triggered";
    pub const RECONCILIATION_REFUSED: &str = "mesh.steward_reconciliation_refused";
    pub const RECONCILIATION_DAILY_CAP_REACHED: &str =
        "mesh.steward_reconciliation_daily_cap_reached";
}

/// Default reconciliation interval (15 minutes), overridable via
/// `EE_MESH_STEWARD_RECONCILIATION_INTERVAL_SECONDS`.
pub const STEWARD_DEFAULT_INTERVAL_SECONDS: u64 = 900;

/// Default jitter (±60 seconds), overridable via
/// `EE_MESH_STEWARD_RECONCILIATION_JITTER_SECONDS`. Avoids the
/// "discovery thundering herd" where every machine on the tailnet
/// reconciles at the same wall-clock second.
pub const STEWARD_DEFAULT_JITTER_SECONDS: u64 = 60;

/// Default per-day reconciliation cap (100), overridable via
/// `EE_MESH_STEWARD_RECONCILIATION_MAX_DAILY`. Prevents a buggy
/// interval setting from running thousands of reconciliations.
pub const STEWARD_DEFAULT_MAX_DAILY: u64 = 100;

// ============================================================================
// Input vocabulary
// ============================================================================

/// SRR6.46.4 drift severity classes the steward consults.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftSeverity {
    /// `discovery == materialized` exactly and no soft-stale peers.
    None,
    /// ≤2 new peers OR transient unreachable (soft_stale).
    Info,
    /// >2 new peers OR hard-stale peers. Actionable.
    Warning,
    /// `tailnetChanged` or `manualConflictPresent`. The steward MUST
    /// refuse to auto-resolve these — they are explicit user-action
    /// signals.
    Medium,
}

impl DriftSeverity {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Medium => "medium",
        }
    }
}

/// What is causing the drift. Discriminates between the
/// auto-resolvable `Warning` cases and selects the audit-reason
/// string the steward emits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    /// `discovery` lists peers not in `materialized`. The steward
    /// runs `ee mesh auto-enroll` to absorb them.
    NewPeersAvailable,
    /// Hard-stale peers in `materialized` per SRR6.46.13 grace
    /// period. The steward runs `ee mesh auto-enroll` to remove them.
    StalePeersInConfig,
    /// `tailnetChanged` — the host's tailnet bound differs from the
    /// one auto-enrollment was materialized against. Steward refuses;
    /// this is SRR6.46.8's user-action territory.
    TailnetChanged,
    /// `manualConflictPresent` — the user has hand-edited the
    /// peer-group binding since the last auto-enrollment. Steward
    /// refuses; auto-overwriting manual edits would violate consent.
    ManualConflictPresent,
    /// No actionable drift. Steward no-ops.
    None,
}

impl DriftKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NewPeersAvailable => "new_peers_available",
            Self::StalePeersInConfig => "stale_peers_in_config",
            Self::TailnetChanged => "tailnet_changed",
            Self::ManualConflictPresent => "manual_conflict_present",
            Self::None => "none",
        }
    }
}

/// Inputs to [`decide_steward_outcome`]. Pure-data; no `&Cx`, no IO.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StewardDecisionInput {
    /// Whether `EE_MESH_AUTO_ENROLL_ON_DEMAND=1` is set. When false,
    /// the steward unconditionally no-ops with [`StewardOutcome::NotEnabled`].
    pub enabled: bool,
    pub drift_severity: DriftSeverity,
    pub drift_kind: DriftKind,
    /// Number of reconciliations the steward has already run today,
    /// per the state file's daily counter.
    pub reconciliations_today: u64,
    /// Per-day cap from `EE_MESH_STEWARD_RECONCILIATION_MAX_DAILY`.
    pub max_daily: u64,
}

// ============================================================================
// Outputs
// ============================================================================

/// Outcome of one steward pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StewardOutcome {
    /// `EE_MESH_AUTO_ENROLL_ON_DEMAND` is not set. The steward unwinds
    /// without consulting anything else.
    NotEnabled,
    /// Drift severity is `none` or `info`; no reconciliation needed.
    /// Emits `mesh.steward_reconciliation_skipped` with
    /// `reason: no_actionable_drift`.
    NoOp,
    /// Drift severity is `warning` AND drift kind is auto-resolvable.
    /// Emits `mesh.steward_reconciliation_triggered` with one of:
    /// `reason: new_peers`, `reason: stale_peers`. The caller runs
    /// `ee mesh auto-enroll` AFTER recording the audit row.
    Triggered,
    /// Drift severity is `medium`. Emits
    /// `mesh.steward_reconciliation_refused` with
    /// `reason: requires_user_action`. The steward MUST NOT touch
    /// peer-group config in this state — the user has to intervene.
    Refused,
    /// Today's reconciliation count has reached the cap. Emits
    /// `mesh.steward_reconciliation_daily_cap_reached`. Cap check
    /// runs before drift evaluation: a buggy interval should not
    /// thrash the audit log with one row per second.
    DailyCapReached,
}

impl StewardOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotEnabled => "not_enabled",
            Self::NoOp => "no_op",
            Self::Triggered => "triggered",
            Self::Refused => "refused",
            Self::DailyCapReached => "daily_cap_reached",
        }
    }

    /// Map an outcome to the audit event type the caller emits.
    /// Returns `None` for [`StewardOutcome::NotEnabled`] — when the
    /// feature is off, the steward unwinds silently rather than
    /// flooding the audit log on every scheduler tick.
    #[must_use]
    pub fn audit_event_type(self) -> Option<&'static str> {
        match self {
            Self::NotEnabled => None,
            Self::NoOp => Some(audit_events::RECONCILIATION_SKIPPED),
            Self::Triggered => Some(audit_events::RECONCILIATION_TRIGGERED),
            Self::Refused => Some(audit_events::RECONCILIATION_REFUSED),
            Self::DailyCapReached => Some(audit_events::RECONCILIATION_DAILY_CAP_REACHED),
        }
    }
}

/// Canonical reason string the steward emits alongside the audit row.
/// Held as constants because downstream surfaces (status, doctor)
/// pattern-match on these strings.
pub mod reasons {
    pub const NO_ACTIONABLE_DRIFT: &str = "no_actionable_drift";
    pub const NEW_PEERS: &str = "new_peers";
    pub const STALE_PEERS: &str = "stale_peers";
    pub const REQUIRES_USER_ACTION: &str = "requires_user_action";
    pub const DAILY_CAP_REACHED: &str = "daily_cap_reached";
    pub const NOT_ENABLED: &str = "not_enabled";
}

/// Outcome plus the canonical reason string the caller threads into
/// the audit row's `details.reason` field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StewardDecision {
    pub outcome: StewardOutcome,
    pub reason: &'static str,
}

// ============================================================================
// Core decision function
// ============================================================================

/// Decide what the steward should do given a resolved input. Pure:
/// no IO, no audit emission, no scheduler interaction. The caller
/// records the audit row, runs `ee mesh auto-enroll` for
/// [`StewardOutcome::Triggered`], and bumps the daily counter.
///
/// Evaluation order (load-bearing):
/// 1. `enabled == false` → [`StewardOutcome::NotEnabled`]. Skip all
///    further checks; the steward is effectively a no-op.
/// 2. `reconciliations_today >= max_daily` → [`StewardOutcome::DailyCapReached`].
///    The cap check runs BEFORE drift evaluation so a buggy interval
///    cannot thrash the audit log with one row per second when
///    something is wrong.
/// 3. `drift_severity == Medium` → [`StewardOutcome::Refused`]. This
///    takes priority over a coincidentally non-actionable drift_kind
///    because Medium is the "explicit user-action signal" tier.
/// 4. `drift_severity == None | Info` → [`StewardOutcome::NoOp`].
/// 5. `drift_severity == Warning`:
///    - `drift_kind == NewPeersAvailable` → [`StewardOutcome::Triggered`]
///      with `reason: new_peers`.
///    - `drift_kind == StalePeersInConfig` → [`StewardOutcome::Triggered`]
///      with `reason: stale_peers`.
///    - any other kind → [`StewardOutcome::NoOp`] (defensive default;
///      severity claimed warning but kind is non-actionable, so the
///      steward declines rather than guess).
#[must_use]
pub fn decide_steward_outcome(input: &StewardDecisionInput) -> StewardDecision {
    if !input.enabled {
        return StewardDecision {
            outcome: StewardOutcome::NotEnabled,
            reason: reasons::NOT_ENABLED,
        };
    }

    if input.reconciliations_today >= input.max_daily {
        return StewardDecision {
            outcome: StewardOutcome::DailyCapReached,
            reason: reasons::DAILY_CAP_REACHED,
        };
    }

    if input.drift_severity == DriftSeverity::Medium {
        return StewardDecision {
            outcome: StewardOutcome::Refused,
            reason: reasons::REQUIRES_USER_ACTION,
        };
    }

    match input.drift_severity {
        DriftSeverity::None | DriftSeverity::Info => StewardDecision {
            outcome: StewardOutcome::NoOp,
            reason: reasons::NO_ACTIONABLE_DRIFT,
        },
        DriftSeverity::Warning => match input.drift_kind {
            DriftKind::NewPeersAvailable => StewardDecision {
                outcome: StewardOutcome::Triggered,
                reason: reasons::NEW_PEERS,
            },
            DriftKind::StalePeersInConfig => StewardDecision {
                outcome: StewardOutcome::Triggered,
                reason: reasons::STALE_PEERS,
            },
            // Severity claims warning but kind isn't auto-resolvable;
            // decline rather than guess. The state machine staying
            // honest matters more than chasing every drift signal.
            _ => StewardDecision {
                outcome: StewardOutcome::NoOp,
                reason: reasons::NO_ACTIONABLE_DRIFT,
            },
        },
        DriftSeverity::Medium => unreachable!("medium handled above"),
    }
}

// ============================================================================
// Interval jitter
// ============================================================================

/// Compute the next-fire delay given a base interval and a jitter
/// window. Pure: the caller supplies the jitter value (in the
/// inclusive range `[-jitter_seconds, +jitter_seconds]`) so tests
/// can pin a deterministic seed without depending on system RNG.
///
/// Returns the effective delay clamped to a minimum of 1 second:
/// even with a worst-case negative jitter, the steward should not
/// fire back-to-back.
#[must_use]
pub fn apply_interval_jitter(
    base_interval_seconds: u64,
    jitter_window_seconds: u64,
    raw_jitter_signed: i64,
) -> u64 {
    let clamped_jitter = clamp_jitter_to_window(raw_jitter_signed, jitter_window_seconds);
    let signed_base = i128::from(base_interval_seconds);
    let adjusted = signed_base.saturating_add(i128::from(clamped_jitter));
    let bounded = adjusted.max(1);
    u64::try_from(bounded).unwrap_or(u64::MAX)
}

fn clamp_jitter_to_window(raw: i64, window: u64) -> i64 {
    let window_signed = i64::try_from(window).unwrap_or(i64::MAX);
    raw.clamp(-window_signed, window_signed)
}

// ============================================================================
// Inline tests (AGENTS.md L300-302 / bd-3usjw.62 Rule 7)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        enabled: bool,
        severity: DriftSeverity,
        kind: DriftKind,
        reconciliations_today: u64,
        max_daily: u64,
    ) -> StewardDecisionInput {
        StewardDecisionInput {
            enabled,
            drift_severity: severity,
            drift_kind: kind,
            reconciliations_today,
            max_daily,
        }
    }

    // ---- enabled gate ------------------------------------------------------

    #[test]
    fn disabled_steward_short_circuits_and_emits_no_audit() {
        let decision = decide_steward_outcome(&input(
            false,
            DriftSeverity::Warning,
            DriftKind::NewPeersAvailable,
            0,
            STEWARD_DEFAULT_MAX_DAILY,
        ));
        assert_eq!(decision.outcome, StewardOutcome::NotEnabled);
        assert_eq!(decision.reason, reasons::NOT_ENABLED);
        // Audit event type is None — disabled state should not flood the log.
        assert!(decision.outcome.audit_event_type().is_none());
    }

    // ---- daily cap runs BEFORE drift evaluation ---------------------------

    #[test]
    fn daily_cap_reached_short_circuits_even_when_drift_actionable() {
        let decision = decide_steward_outcome(&input(
            true,
            DriftSeverity::Warning,
            DriftKind::NewPeersAvailable,
            100,
            100,
        ));
        assert_eq!(decision.outcome, StewardOutcome::DailyCapReached);
        assert_eq!(decision.reason, reasons::DAILY_CAP_REACHED);
        assert_eq!(
            decision.outcome.audit_event_type(),
            Some(audit_events::RECONCILIATION_DAILY_CAP_REACHED)
        );
    }

    #[test]
    fn daily_cap_zero_means_never_reconcile() {
        let decision = decide_steward_outcome(&input(
            true,
            DriftSeverity::Warning,
            DriftKind::NewPeersAvailable,
            0,
            0,
        ));
        assert_eq!(decision.outcome, StewardOutcome::DailyCapReached);
    }

    // ---- medium severity ALWAYS refuses -----------------------------------

    #[test]
    fn medium_severity_with_tailnet_change_refuses() {
        let decision = decide_steward_outcome(&input(
            true,
            DriftSeverity::Medium,
            DriftKind::TailnetChanged,
            0,
            100,
        ));
        assert_eq!(decision.outcome, StewardOutcome::Refused);
        assert_eq!(decision.reason, reasons::REQUIRES_USER_ACTION);
        assert_eq!(
            decision.outcome.audit_event_type(),
            Some(audit_events::RECONCILIATION_REFUSED)
        );
    }

    #[test]
    fn medium_severity_with_manual_conflict_refuses() {
        let decision = decide_steward_outcome(&input(
            true,
            DriftSeverity::Medium,
            DriftKind::ManualConflictPresent,
            0,
            100,
        ));
        assert_eq!(decision.outcome, StewardOutcome::Refused);
    }

    #[test]
    fn medium_severity_does_not_trip_triggered_even_with_actionable_kind() {
        // Inputs are contradictory by spec; ensure the severity wins.
        let decision = decide_steward_outcome(&input(
            true,
            DriftSeverity::Medium,
            DriftKind::NewPeersAvailable,
            0,
            100,
        ));
        assert_eq!(decision.outcome, StewardOutcome::Refused);
    }

    // ---- info / none → NoOp ------------------------------------------------

    #[test]
    fn severity_none_yields_noop() {
        let decision = decide_steward_outcome(&input(
            true,
            DriftSeverity::None,
            DriftKind::None,
            5,
            100,
        ));
        assert_eq!(decision.outcome, StewardOutcome::NoOp);
        assert_eq!(decision.reason, reasons::NO_ACTIONABLE_DRIFT);
        assert_eq!(
            decision.outcome.audit_event_type(),
            Some(audit_events::RECONCILIATION_SKIPPED)
        );
    }

    #[test]
    fn severity_info_yields_noop_even_with_new_peers_kind() {
        // Info severity captures ≤2 new peers — below the actionable bar.
        let decision = decide_steward_outcome(&input(
            true,
            DriftSeverity::Info,
            DriftKind::NewPeersAvailable,
            5,
            100,
        ));
        assert_eq!(decision.outcome, StewardOutcome::NoOp);
    }

    // ---- warning + actionable kind → Triggered ----------------------------

    #[test]
    fn warning_new_peers_triggers_with_new_peers_reason() {
        let decision = decide_steward_outcome(&input(
            true,
            DriftSeverity::Warning,
            DriftKind::NewPeersAvailable,
            5,
            100,
        ));
        assert_eq!(decision.outcome, StewardOutcome::Triggered);
        assert_eq!(decision.reason, reasons::NEW_PEERS);
        assert_eq!(
            decision.outcome.audit_event_type(),
            Some(audit_events::RECONCILIATION_TRIGGERED)
        );
    }

    #[test]
    fn warning_stale_peers_triggers_with_stale_peers_reason() {
        let decision = decide_steward_outcome(&input(
            true,
            DriftSeverity::Warning,
            DriftKind::StalePeersInConfig,
            5,
            100,
        ));
        assert_eq!(decision.outcome, StewardOutcome::Triggered);
        assert_eq!(decision.reason, reasons::STALE_PEERS);
    }

    #[test]
    fn warning_with_none_kind_declines_rather_than_guess() {
        // Defensive: severity says warning but kind says nothing actionable.
        let decision = decide_steward_outcome(&input(
            true,
            DriftSeverity::Warning,
            DriftKind::None,
            5,
            100,
        ));
        assert_eq!(decision.outcome, StewardOutcome::NoOp);
    }

    // ---- Schema constant + enum strings ------------------------------------

    #[test]
    fn schema_constant_matches_documented_version() {
        assert_eq!(STEWARD_STATUS_SCHEMA_V1, "ee.mesh.steward.status.v1");
    }

    #[test]
    fn enum_strings_match_snake_case_serde() {
        for variant in [
            DriftSeverity::None,
            DriftSeverity::Info,
            DriftSeverity::Warning,
            DriftSeverity::Medium,
        ] {
            let serialized = serde_json::to_string(&variant).expect("serialize");
            assert!(serialized.contains(variant.as_str()), "{serialized}");
        }
        for variant in [
            DriftKind::NewPeersAvailable,
            DriftKind::StalePeersInConfig,
            DriftKind::TailnetChanged,
            DriftKind::ManualConflictPresent,
            DriftKind::None,
        ] {
            let serialized = serde_json::to_string(&variant).expect("serialize");
            assert!(serialized.contains(variant.as_str()), "{serialized}");
        }
        for variant in [
            StewardOutcome::NotEnabled,
            StewardOutcome::NoOp,
            StewardOutcome::Triggered,
            StewardOutcome::Refused,
            StewardOutcome::DailyCapReached,
        ] {
            let serialized = serde_json::to_string(&variant).expect("serialize");
            assert!(serialized.contains(variant.as_str()), "{serialized}");
        }
    }

    // ---- Jitter ------------------------------------------------------------

    #[test]
    fn jitter_zero_returns_base_interval_unchanged() {
        assert_eq!(apply_interval_jitter(900, 60, 0), 900);
    }

    #[test]
    fn jitter_positive_within_window_adds_to_base() {
        assert_eq!(apply_interval_jitter(900, 60, 30), 930);
    }

    #[test]
    fn jitter_negative_within_window_subtracts_from_base() {
        assert_eq!(apply_interval_jitter(900, 60, -30), 870);
    }

    #[test]
    fn jitter_at_window_edge_is_honored() {
        assert_eq!(apply_interval_jitter(900, 60, 60), 960);
        assert_eq!(apply_interval_jitter(900, 60, -60), 840);
    }

    #[test]
    fn jitter_beyond_window_is_clamped() {
        assert_eq!(apply_interval_jitter(900, 60, 1000), 960);
        assert_eq!(apply_interval_jitter(900, 60, -1000), 840);
    }

    #[test]
    fn jitter_clamps_to_minimum_one_second_even_at_negative_extreme() {
        // Tiny base interval, large jitter window — must still fire eventually.
        assert_eq!(apply_interval_jitter(5, 100, -100), 1);
    }

    #[test]
    fn jitter_does_not_overflow_with_huge_inputs() {
        let result = apply_interval_jitter(u64::MAX, 1000, 999);
        // Saturates at u64::MAX rather than panicking.
        assert_eq!(result, u64::MAX);
    }
}
