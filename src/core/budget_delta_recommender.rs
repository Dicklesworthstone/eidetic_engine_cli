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
use std::path::Path;

use serde::{Serialize, Serializer};

use super::profile::{
    HOST_PROFILE_PROBE_SCHEMA_V1, HostCalibrationFreshness, HostClass, HostClassDegradation,
    HostClassRepairAction, HostClassReport, HostClassificationOptions, HostResourceProbeReport,
    OperatingProfile, classify_host_profile,
};

/// Public schema identifier for the recommender's response shape. Surfaces
/// that embed the recommendation (status, doctor, support bundle, swarm brief)
/// MUST hold this constant rather than redeclaring the string literal so the
/// schema lifecycle can register it in one place.
pub const BUDGET_DELTA_RECOMMENDATION_SCHEMA_V1: &str = "ee.host_calibration.recommendation.v1";
pub const HOST_CALIBRATION_POSTURE_SCHEMA_V1: &str = "ee.host_calibration.posture.v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCalibrationPostureReport {
    pub schema: &'static str,
    pub redaction_status: &'static str,
    pub host_profile_schema: &'static str,
    pub host_class: HostClass,
    pub calibration_freshness: HostCalibrationFreshness,
    pub confidence: &'static str,
    pub profile_ceiling: OperatingProfile,
    pub configured_profile: OperatingProfile,
    pub recommended_profile: OperatingProfile,
    pub effective_profile: OperatingProfile,
    pub target_dir_posture: &'static str,
    pub topology_warnings: Vec<&'static str>,
    pub reason_codes: Vec<&'static str>,
    pub repair_actions: Vec<HostClassRepairAction>,
    pub budget_deltas: Vec<BudgetDelta>,
    pub degraded: Vec<HostClassDegradation>,
}

/// Stable reason-code vocabulary. Listed here so contract tests (H3.5) can pin
/// the set without relying on free-form strings produced by the recommender.
pub mod reason_code {
    pub const NO_CHANGE: &str = "no_change";
    pub const ELEVATE_TO_RECOMMENDED: &str = "elevate_to_recommended_profile";
    pub const LOWER_TO_RECOMMENDED: &str = "lower_to_recommended_profile";
    pub const CONSERVATIVE_CALIBRATION_STALE: &str = "conservative_calibration_stale";
    pub const CONSERVATIVE_CALIBRATION_PARTIAL: &str = "conservative_calibration_partial";
    pub const CONSERVATIVE_CALIBRATION_SYNTHETIC_ONLY: &str =
        "conservative_calibration_synthetic_only";
    pub const CONSERVATIVE_CALIBRATION_CONTRADICTORY: &str =
        "conservative_calibration_contradictory";
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
        self.effective_profile != self.configured_profile
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
    pub degraded: Vec<HostClassDegradation>,
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
        degraded: host_class_report.degraded.clone(),
    }
}

#[must_use]
pub fn gather_host_calibration_posture(
    workspace: &Path,
    configured_profile: OperatingProfile,
) -> HostCalibrationPostureReport {
    let probe = HostResourceProbeReport::gather_for_workspace(workspace);
    build_host_calibration_posture(&probe, configured_profile)
}

#[must_use]
pub fn build_host_calibration_posture(
    probe: &HostResourceProbeReport,
    configured_profile: OperatingProfile,
) -> HostCalibrationPostureReport {
    let host_class_report = classify_host_profile(probe, &HostClassificationOptions::live_probe());
    let recommendation = recommend_budget_deltas(&host_class_report, configured_profile);
    let mut reason_codes = host_class_report
        .reason_codes
        .iter()
        .copied()
        .chain(recommendation.global_reason_codes.iter().copied())
        .collect::<Vec<_>>();
    reason_codes.sort_unstable();
    reason_codes.dedup();

    HostCalibrationPostureReport {
        schema: HOST_CALIBRATION_POSTURE_SCHEMA_V1,
        redaction_status: "label_only_paths_presence_only_env_no_raw_values",
        host_profile_schema: HOST_PROFILE_PROBE_SCHEMA_V1,
        host_class: host_class_report.host_class,
        calibration_freshness: host_class_report.calibration_freshness,
        confidence: host_class_report.confidence,
        profile_ceiling: host_class_report.profile_ceiling,
        configured_profile,
        recommended_profile: recommendation.recommended_profile,
        effective_profile: recommendation.effective_profile,
        target_dir_posture: target_dir_posture(probe),
        topology_warnings: topology_warnings(&host_class_report, probe),
        reason_codes,
        repair_actions: host_class_report.repair_actions,
        budget_deltas: recommendation.budget_deltas,
        degraded: recommendation.degraded,
    }
}

fn target_dir_posture(probe: &HostResourceProbeReport) -> &'static str {
    let cargo_target = probe.paths.iter().find(|path| path.label == "cargo_target");
    match cargo_target.and_then(|path| path.same_filesystem_as_workspace) {
        Some(false) if probe.environment.cargo_target_dir_configured => "external",
        Some(false) => "isolated",
        Some(true) => "shared",
        None => "unknown",
    }
}

fn topology_warnings(
    report: &HostClassReport,
    probe: &HostResourceProbeReport,
) -> Vec<&'static str> {
    let mut warnings = Vec::new();
    if !probe.topology.rch.available {
        warnings.push("rch_topology_missing");
    }
    if report.host_class == HostClass::RchOnlyTopology {
        warnings.push("rch_only_topology_blocks_local_classification");
    }
    warnings.sort_unstable();
    warnings.dedup();
    warnings
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
        HostCalibrationFreshness::Partial => {
            reasons.push(reason_code::CONSERVATIVE_CALIBRATION_PARTIAL);
            OperatingProfile::Portable
        }
        HostCalibrationFreshness::SyntheticOnly => {
            reasons.push(reason_code::CONSERVATIVE_CALIBRATION_SYNTHETIC_ONLY);
            OperatingProfile::Portable
        }
        HostCalibrationFreshness::Contradictory => {
            reasons.push(reason_code::CONSERVATIVE_CALIBRATION_CONTRADICTORY);
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
    let reason_code = match effective_profile.cmp(&configured_profile) {
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
    use super::super::profile::{
        CpuProbe, EnvironmentProbe, HOST_PROFILE_PROBE_SCHEMA_V1, HostTopologyProbe, MemoryProbe,
        PathCapacityProbe, RchTopologyProbe, WorkspaceProbe,
    };
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn recommendation_schema_id_matches_registered_contract() {
        let report = host_class_report(
            HostClass::Workstation,
            OperatingProfile::Workstation,
            HostCalibrationFreshness::Fresh,
        );
        let recommendation = recommend_budget_deltas(&report, OperatingProfile::Workstation);

        assert_eq!(
            BUDGET_DELTA_RECOMMENDATION_SCHEMA_V1,
            "ee.host_calibration.recommendation.v1"
        );
        assert_eq!(recommendation.schema, BUDGET_DELTA_RECOMMENDATION_SCHEMA_V1);
    }

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
            degraded: Vec::new(),
        }
    }

    fn live_probe(
        logical_cores: u32,
        memory_gib: u64,
        cargo_target_external: bool,
    ) -> HostResourceProbeReport {
        HostResourceProbeReport {
            schema: HOST_PROFILE_PROBE_SCHEMA_V1,
            side_effect_free: true,
            redaction: "label_only_paths_presence_only_env",
            complete: true,
            workspace: WorkspaceProbe {
                label: "workspace",
                initialized: true,
                redaction: "path_not_emitted",
            },
            cpu: CpuProbe {
                logical_cores: Some(logical_cores),
                physical_cores: Some(logical_cores.saturating_div(2).max(1)),
                source: "unit_test",
            },
            memory: MemoryProbe {
                total_bytes: Some(memory_gib * GIB),
                available_bytes: Some(memory_gib * GIB),
                cgroup_limit_bytes: None,
                source: "unit_test",
            },
            paths: vec![PathCapacityProbe {
                label: "cargo_target",
                role: "cargo_target_dir",
                path: None,
                exists: Some(true),
                probe_status: "observed",
                nearest_existing_ancestor: Some(false),
                same_filesystem_as_workspace: Some(!cargo_target_external),
                total_bytes: Some(512 * GIB),
                available_bytes: Some(512 * GIB),
                redaction: "path_not_emitted",
            }],
            tools: Vec::new(),
            environment: EnvironmentProbe {
                tmpdir_configured: true,
                cargo_target_dir_configured: cargo_target_external,
                rch_hint_configured: false,
                redaction: "presence_only",
            },
            topology: HostTopologyProbe {
                rch: RchTopologyProbe {
                    available: true,
                    status: "available_not_queried",
                    posture: "ok",
                    source: "unit_test",
                    message: "RCH available for live-probe unit test.".to_owned(),
                    repair: None,
                },
            },
            degraded: Vec::new(),
        }
    }

    #[test]
    fn live_gathered_posture_uses_fresh_calibration_not_missing_fallback() {
        let probe = live_probe(32, 256, true);
        let posture = build_host_calibration_posture(&probe, OperatingProfile::Swarm);

        assert_eq!(
            posture.calibration_freshness,
            HostCalibrationFreshness::Fresh
        );
        assert_eq!(posture.host_class, HostClass::Local256Gb);
        assert_eq!(posture.recommended_profile, OperatingProfile::Swarm);
        assert_eq!(posture.effective_profile, OperatingProfile::Swarm);
        assert!(posture.budget_deltas.iter().all(|delta| {
            delta.reason_code == reason_code::NO_CHANGE
                && delta.effective_profile == OperatingProfile::Swarm
        }));
        assert!(
            !posture
                .reason_codes
                .iter()
                .any(|code| code.contains("calibration_missing")),
            "live gathered posture must not report missing calibration: {:?}",
            posture.reason_codes
        );
        assert!(
            posture.degraded.is_empty(),
            "fresh live gathered posture must not emit calibration degradations: {:?}",
            posture.degraded
        );
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
    fn no_change_when_profile_ceiling_matches_configured_profile() {
        let report = host_class_report(
            HostClass::Local256Gb,
            OperatingProfile::Workstation,
            HostCalibrationFreshness::Fresh,
        );
        let recommendation = recommend_budget_deltas(&report, OperatingProfile::Workstation);

        assert_eq!(recommendation.recommended_profile, OperatingProfile::Swarm);
        assert_eq!(
            recommendation.effective_profile,
            OperatingProfile::Workstation
        );
        assert!(!recommendation.any_changes_recommended());
        for delta in &recommendation.budget_deltas {
            assert_eq!(delta.configured_profile, OperatingProfile::Workstation);
            assert_eq!(delta.effective_profile, OperatingProfile::Workstation);
            assert_eq!(delta.reason_code, reason_code::NO_CHANGE);
            assert!(!delta.would_change());
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

    #[test]
    fn contradictory_calibration_forces_conservative_profile() {
        let report = host_class_report(
            HostClass::Local256Gb,
            OperatingProfile::Swarm,
            HostCalibrationFreshness::Contradictory,
        );
        let recommendation = recommend_budget_deltas(&report, OperatingProfile::Workstation);

        assert_eq!(
            recommendation.recommended_profile,
            OperatingProfile::Portable
        );
        assert!(
            recommendation
                .global_reason_codes
                .contains(&reason_code::CONSERVATIVE_CALIBRATION_CONTRADICTORY)
        );
    }

    #[test]
    fn recommender_forwards_host_class_degradations() {
        let mut report = host_class_report(
            HostClass::Workstation,
            OperatingProfile::Workstation,
            HostCalibrationFreshness::Partial,
        );
        report.degraded.push(HostClassDegradation {
            code: "host_calibration_partial",
            severity: "warning",
            message: "partial calibration",
            repair: Some("rch exec -- scripts/e2e_overhaul/host_calibration.sh"),
        });

        let recommendation = recommend_budget_deltas(&report, OperatingProfile::Workstation);

        assert_eq!(recommendation.degraded, report.degraded);
    }
}
