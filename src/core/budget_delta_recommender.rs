//! H3.3 (bd-1zb7k.12.3.3): pure budget-delta recommender that maps a host-class
//! classification into concrete per-surface budgets without mutating
//! configuration.
//!
//! Acceptance shape pinned by the bead body:
//! - Recommend context pack, cache sizing, graph snapshot memory, index rebuild
//!   concurrency, and burst admission budgets.
//! - Emit a `budgetDeltas[]` block carrying `configuredProfile`,
//!   `recommendedProfile`, `effectiveProfile`, stable numeric units, and
//!   machine-readable reason codes.
//! - Be conservative when calibration freshness or RCH topology degrades.
//! - Include large-host behavior for 256 GB / 64-core swarms without raising
//!   the floor for smaller machines.
//! - Be explainable enough that `ee status`, `ee doctor`, and support bundles
//!   can render the deltas compactly in H4.
//!
//! This module is intentionally side-effect free and DB-free. The actual
//! application of any recommended profile is owned by a future H3.4 wiring;
//! this recommender never claims to have already applied a profile.

use std::cmp::Ordering;

use serde::{Serialize, Serializer};

use super::profile::{HostCalibrationFreshness, HostClass, HostClassReport, OperatingProfile};

/// Public schema identifier for the recommender's response shape. Surfaces
/// that embed the recommendation (status, doctor, support bundle, swarm brief)
/// MUST hold this constant rather than redeclaring the string literal so the
/// schema lifecycle can register it in one place.
pub const BUDGET_DELTA_RECOMMENDATION_SCHEMA_V1: &str = "ee.host_calibration.budget_delta.v1";

/// Stable reason-code vocabulary. Listed here so contract tests (H3.5) can pin
/// the set without relying on free-form strings produced by the recommender.
pub mod reason_code {
    pub const NO_CHANGE: &str = "no_change";
    pub const ELEVATE_TO_RECOMMENDED: &str = "elevate_to_recommended_profile";
    pub const LOWER_TO_RECOMMENDED: &str = "lower_to_recommended_profile";
    pub const CONSERVATIVE_CALIBRATION_STALE: &str = "conservative_calibration_stale";
    pub const CONSERVATIVE_CALIBRATION_MISSING: &str = "conservative_calibration_missing";
    pub const CONSERVATIVE_CALIBRATION_UNAVAILABLE: &str = "conservative_calibration_unavailable";
    pub const CONSERVATIVE_RCH_ONLY_TOPOLOGY: &str = "conservative_rch_only_topology";
    pub const SWARM_HOST_HEADROOM: &str = "swarm_host_headroom";
    pub const PROFILE_CEILING_CLAMPED: &str = "profile_ceiling_clamped";
}

/// Identifier for each subsystem the recommender produces a budget delta for.
/// The set is closed; H3.5 contract tests pin coverage of every variant.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BudgetSurface {
    ContextPack,
    Cache,
    GraphSnapshot,
    IndexRebuild,
    BurstAdmission,
}

impl BudgetSurface {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContextPack => "context_pack",
            Self::Cache => "cache",
            Self::GraphSnapshot => "graph_snapshot",
            Self::IndexRebuild => "index_rebuild",
            Self::BurstAdmission => "burst_admission",
        }
    }

    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::ContextPack => "tokens",
            Self::Cache => "bytes",
            Self::GraphSnapshot => "bytes",
            Self::IndexRebuild => "concurrent_jobs",
            Self::BurstAdmission => "queued_admissions",
        }
    }
}

impl Serialize for BudgetSurface {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// One per-surface budget delta entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetDelta {
    pub surface: BudgetSurface,
    pub unit: &'static str,
    pub configured_profile: OperatingProfile,
    pub recommended_profile: OperatingProfile,
    pub effective_profile: OperatingProfile,
    pub configured_value: u64,
    pub recommended_value: u64,
    pub effective_value: u64,
    pub reason_code: &'static str,
}

impl BudgetDelta {
    /// Whether the recommender is asking the caller to change this surface.
    /// When false, the entry exists as evidence that the surface was
    /// considered and no change is needed.
    #[must_use]
    pub fn would_change(&self) -> bool {
        self.recommended_profile != self.configured_profile
    }
}

/// Recommender response carrying every per-surface delta plus the global
/// recommendation context. Holds no DB handles and performs no I/O.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetDeltaRecommendation {
    pub schema: &'static str,
    pub side_effect_free: bool,
    pub host_class: HostClass,
    pub calibration_freshness: HostCalibrationFreshness,
    pub configured_profile: OperatingProfile,
    pub recommended_profile: OperatingProfile,
    pub effective_profile: OperatingProfile,
    pub global_reason_codes: Vec<&'static str>,
    pub budget_deltas: Vec<BudgetDelta>,
}

impl BudgetDeltaRecommendation {
    /// True iff at least one surface has a recommended change.
    #[must_use]
    pub fn any_changes_recommended(&self) -> bool {
        self.budget_deltas.iter().any(BudgetDelta::would_change)
    }
}

/// Pure recommender entrypoint. The host-class report describes WHAT the
/// machine is; the configured profile describes WHAT ee is currently using;
/// the function returns deltas explaining the gap.
#[must_use]
pub fn recommend_budget_deltas(
    host_class_report: &HostClassReport,
    configured_profile: OperatingProfile,
) -> BudgetDeltaRecommendation {
    let (recommended_profile, mut global_reasons) = derive_recommended_profile(host_class_report);
    let effective_profile = recommended_profile.min(host_class_report.profile_ceiling);
    if effective_profile != recommended_profile {
        global_reasons.push(reason_code::PROFILE_CEILING_CLAMPED);
    }

    let budget_deltas = BUDGET_SURFACES
        .iter()
        .copied()
        .map(|surface| {
            build_budget_delta(
                surface,
                configured_profile,
                recommended_profile,
                effective_profile,
            )
        })
        .collect();

    BudgetDeltaRecommendation {
        schema: BUDGET_DELTA_RECOMMENDATION_SCHEMA_V1,
        side_effect_free: true,
        host_class: host_class_report.host_class,
        calibration_freshness: host_class_report.calibration_freshness,
        configured_profile,
        recommended_profile,
        effective_profile,
        global_reason_codes: global_reasons,
        budget_deltas,
    }
}

const BUDGET_SURFACES: &[BudgetSurface] = &[
    BudgetSurface::ContextPack,
    BudgetSurface::Cache,
    BudgetSurface::GraphSnapshot,
    BudgetSurface::IndexRebuild,
    BudgetSurface::BurstAdmission,
];

fn derive_recommended_profile(
    host_class_report: &HostClassReport,
) -> (OperatingProfile, Vec<&'static str>) {
    let mut reasons: Vec<&'static str> = Vec::new();
    let class_floor = match host_class_report.host_class {
        HostClass::Constrained => OperatingProfile::Constrained,
        HostClass::Portable => OperatingProfile::Portable,
        HostClass::Laptop => OperatingProfile::Workstation,
        HostClass::Workstation => OperatingProfile::Workstation,
        HostClass::Local256Gb => {
            reasons.push(reason_code::SWARM_HOST_HEADROOM);
            OperatingProfile::Swarm
        }
        HostClass::RchOnlyTopology => {
            reasons.push(reason_code::CONSERVATIVE_RCH_ONLY_TOPOLOGY);
            OperatingProfile::Portable
        }
    };
    let calibration_cap = match host_class_report.calibration_freshness {
        HostCalibrationFreshness::Fresh => OperatingProfile::Swarm,
        HostCalibrationFreshness::Stale => {
            reasons.push(reason_code::CONSERVATIVE_CALIBRATION_STALE);
            OperatingProfile::Portable
        }
        HostCalibrationFreshness::Missing => {
            reasons.push(reason_code::CONSERVATIVE_CALIBRATION_MISSING);
            OperatingProfile::Portable
        }
        HostCalibrationFreshness::Unavailable => {
            reasons.push(reason_code::CONSERVATIVE_CALIBRATION_UNAVAILABLE);
            OperatingProfile::Portable
        }
    };
    let recommended = class_floor.min(calibration_cap);
    (recommended, reasons)
}

fn build_budget_delta(
    surface: BudgetSurface,
    configured_profile: OperatingProfile,
    recommended_profile: OperatingProfile,
    effective_profile: OperatingProfile,
) -> BudgetDelta {
    let configured_value = surface_value(surface, configured_profile);
    let recommended_value = surface_value(surface, recommended_profile);
    let effective_value = surface_value(surface, effective_profile);
    let reason_code = match recommended_profile.cmp(&configured_profile) {
        Ordering::Equal => reason_code::NO_CHANGE,
        Ordering::Greater => reason_code::ELEVATE_TO_RECOMMENDED,
        Ordering::Less => reason_code::LOWER_TO_RECOMMENDED,
    };
    BudgetDelta {
        surface,
        unit: surface.unit(),
        configured_profile,
        recommended_profile,
        effective_profile,
        configured_value,
        recommended_value,
        effective_value,
        reason_code,
    }
}

/// Per-surface, per-profile concrete numeric value table. Kept in one place
/// so future surface additions only need to edit this function plus the
/// `BudgetSurface` enum + reason-code vocabulary.
const fn surface_value(surface: BudgetSurface, profile: OperatingProfile) -> u64 {
    match (surface, profile) {
        (BudgetSurface::ContextPack, OperatingProfile::Constrained) => 4_000,
        (BudgetSurface::ContextPack, OperatingProfile::Portable) => 8_000,
        (BudgetSurface::ContextPack, OperatingProfile::Workstation) => 16_000,
        (BudgetSurface::ContextPack, OperatingProfile::Swarm) => 32_000,

        (BudgetSurface::Cache, OperatingProfile::Constrained) => 64 * 1024 * 1024,
        (BudgetSurface::Cache, OperatingProfile::Portable) => 256 * 1024 * 1024,
        (BudgetSurface::Cache, OperatingProfile::Workstation) => 1_024 * 1024 * 1024,
        (BudgetSurface::Cache, OperatingProfile::Swarm) => 4_096 * 1024 * 1024,

        (BudgetSurface::GraphSnapshot, OperatingProfile::Constrained) => 128 * 1024 * 1024,
        (BudgetSurface::GraphSnapshot, OperatingProfile::Portable) => 512 * 1024 * 1024,
        (BudgetSurface::GraphSnapshot, OperatingProfile::Workstation) => 2_048 * 1024 * 1024,
        (BudgetSurface::GraphSnapshot, OperatingProfile::Swarm) => 8_192 * 1024 * 1024,

        (BudgetSurface::IndexRebuild, OperatingProfile::Constrained) => 1,
        (BudgetSurface::IndexRebuild, OperatingProfile::Portable) => 2,
        (BudgetSurface::IndexRebuild, OperatingProfile::Workstation) => 8,
        (BudgetSurface::IndexRebuild, OperatingProfile::Swarm) => 16,

        (BudgetSurface::BurstAdmission, OperatingProfile::Constrained) => 4,
        (BudgetSurface::BurstAdmission, OperatingProfile::Portable) => 8,
        (BudgetSurface::BurstAdmission, OperatingProfile::Workstation) => 32,
        (BudgetSurface::BurstAdmission, OperatingProfile::Swarm) => 64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_class_report(
        host_class: HostClass,
        profile_ceiling: OperatingProfile,
        calibration_freshness: HostCalibrationFreshness,
    ) -> HostClassReport {
        HostClassReport {
            schema: super::super::profile::HOST_CLASSIFICATION_SCHEMA_V1,
            side_effect_free: true,
            host_class,
            profile_ceiling,
            confidence: "exact",
            calibration_freshness,
            reason_codes: Vec::new(),
            repair_actions: Vec::new(),
        }
    }

    #[test]
    fn recommends_swarm_for_local_256gb_with_fresh_calibration() {
        let report = host_class_report(
            HostClass::Local256Gb,
            OperatingProfile::Swarm,
            HostCalibrationFreshness::Fresh,
        );
        let recommendation = recommend_budget_deltas(&report, OperatingProfile::Workstation);
        assert_eq!(recommendation.recommended_profile, OperatingProfile::Swarm);
        assert_eq!(recommendation.effective_profile, OperatingProfile::Swarm);
        assert!(
            recommendation
                .global_reason_codes
                .contains(&reason_code::SWARM_HOST_HEADROOM)
        );
        assert!(recommendation.any_changes_recommended());
        for delta in &recommendation.budget_deltas {
            assert_eq!(delta.recommended_profile, OperatingProfile::Swarm);
            assert!(delta.would_change());
            assert_eq!(delta.reason_code, reason_code::ELEVATE_TO_RECOMMENDED);
        }
    }

    #[test]
    fn caps_recommendation_to_portable_when_calibration_is_missing() {
        let report = host_class_report(
            HostClass::Local256Gb,
            OperatingProfile::Swarm,
            HostCalibrationFreshness::Missing,
        );
        let recommendation = recommend_budget_deltas(&report, OperatingProfile::Workstation);
        assert_eq!(
            recommendation.recommended_profile,
            OperatingProfile::Portable
        );
        assert!(
            recommendation
                .global_reason_codes
                .contains(&reason_code::CONSERVATIVE_CALIBRATION_MISSING)
        );
        for delta in &recommendation.budget_deltas {
            assert_eq!(delta.reason_code, reason_code::LOWER_TO_RECOMMENDED);
        }
    }

    #[test]
    fn rch_only_topology_is_conservative_even_with_fresh_calibration() {
        let report = host_class_report(
            HostClass::RchOnlyTopology,
            OperatingProfile::Swarm,
            HostCalibrationFreshness::Fresh,
        );
        let recommendation = recommend_budget_deltas(&report, OperatingProfile::Workstation);
        assert_eq!(
            recommendation.recommended_profile,
            OperatingProfile::Portable
        );
        assert!(
            recommendation
                .global_reason_codes
                .contains(&reason_code::CONSERVATIVE_RCH_ONLY_TOPOLOGY)
        );
    }

    #[test]
    fn no_change_recommended_when_configured_matches_recommendation() {
        let report = host_class_report(
            HostClass::Workstation,
            OperatingProfile::Workstation,
            HostCalibrationFreshness::Fresh,
        );
        let recommendation = recommend_budget_deltas(&report, OperatingProfile::Workstation);
        assert_eq!(
            recommendation.recommended_profile,
            OperatingProfile::Workstation
        );
        assert!(!recommendation.any_changes_recommended());
        for delta in &recommendation.budget_deltas {
            assert_eq!(delta.reason_code, reason_code::NO_CHANGE);
            assert!(!delta.would_change());
        }
    }

    #[test]
    fn profile_ceiling_clamps_recommendation_below_class_floor() {
        let report = host_class_report(
            HostClass::Local256Gb,
            OperatingProfile::Workstation,
            HostCalibrationFreshness::Fresh,
        );
        let recommendation = recommend_budget_deltas(&report, OperatingProfile::Constrained);
        assert_eq!(recommendation.recommended_profile, OperatingProfile::Swarm);
        assert_eq!(
            recommendation.effective_profile,
            OperatingProfile::Workstation
        );
        assert!(
            recommendation
                .global_reason_codes
                .contains(&reason_code::PROFILE_CEILING_CLAMPED)
        );
        for delta in &recommendation.budget_deltas {
            assert_eq!(delta.effective_profile, OperatingProfile::Workstation);
            assert_ne!(delta.recommended_value, delta.effective_value);
        }
    }

    #[test]
    fn every_budget_surface_is_covered_once() {
        let report = host_class_report(
            HostClass::Laptop,
            OperatingProfile::Workstation,
            HostCalibrationFreshness::Fresh,
        );
        let recommendation = recommend_budget_deltas(&report, OperatingProfile::Portable);
        let mut surfaces: Vec<BudgetSurface> = recommendation
            .budget_deltas
            .iter()
            .map(|delta| delta.surface)
            .collect();
        surfaces.sort();
        surfaces.dedup();
        assert_eq!(surfaces.len(), BUDGET_SURFACES.len());
        assert!(
            surfaces.contains(&BudgetSurface::ContextPack)
                && surfaces.contains(&BudgetSurface::Cache)
                && surfaces.contains(&BudgetSurface::GraphSnapshot)
                && surfaces.contains(&BudgetSurface::IndexRebuild)
                && surfaces.contains(&BudgetSurface::BurstAdmission)
        );
    }

    #[test]
    fn recommender_is_deterministic_across_repeat_calls() {
        let report = host_class_report(
            HostClass::Workstation,
            OperatingProfile::Workstation,
            HostCalibrationFreshness::Stale,
        );
        let first = recommend_budget_deltas(&report, OperatingProfile::Workstation);
        let second = recommend_budget_deltas(&report, OperatingProfile::Workstation);
        assert_eq!(first, second);
    }
}
