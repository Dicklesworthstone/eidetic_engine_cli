//! Property tests for profile probe → profile recommendation mapping.
//!
//! These tests verify that:
//! 1. Profile recommendation is deterministic for the same probe inputs
//! 2. Profile thresholds partition the resource space correctly
//! 3. Budget scaling is monotonic with profile tier

use ee::core::profile::{
    CpuProbe, EnvironmentProbe, HOST_PROFILE_PROBE_SCHEMA_V1, HostResourceProbeReport, MemoryProbe,
    OperatingProfile, PROFILE_BUDGET_CONFORMANCE_SCHEMA_V1, ProfileBudgetConformanceStatus,
    ProfileBudgets, WorkspaceProbe, check_profile_budget_artifact_conformance,
    recommend_operating_profile,
};
use ee::models::{
    ArtifactKind, ArtifactSummary, MetricValue, ProfileReference, SummaryDegradation,
};
use proptest::prelude::*;

const GIB: u64 = 1024 * 1024 * 1024;

fn synthetic_workspace_probe() -> WorkspaceProbe {
    WorkspaceProbe {
        label: "workspace",
        initialized: true,
        redaction: "path_not_emitted",
    }
}

fn synthetic_cpu_probe(logical_cores: u32) -> CpuProbe {
    CpuProbe {
        logical_cores: Some(logical_cores),
        physical_cores: None,
        source: "property_test",
    }
}

fn synthetic_memory_probe(total_gib: u64, available_gib: u64) -> MemoryProbe {
    MemoryProbe {
        total_bytes: Some(total_gib * GIB),
        available_bytes: Some(available_gib * GIB),
        cgroup_limit_bytes: None,
        source: "property_test",
    }
}

fn synthetic_environment_probe() -> EnvironmentProbe {
    EnvironmentProbe {
        tmpdir_configured: true,
        cargo_target_dir_configured: false,
        rch_hint_configured: false,
        redaction: "presence_only",
    }
}

fn synthetic_probe(
    logical_cores: u32,
    total_gib: u64,
    available_gib: u64,
) -> HostResourceProbeReport {
    HostResourceProbeReport {
        schema: HOST_PROFILE_PROBE_SCHEMA_V1,
        side_effect_free: true,
        redaction: "label_only_paths_presence_only_env",
        complete: true,
        workspace: synthetic_workspace_probe(),
        cpu: synthetic_cpu_probe(logical_cores),
        memory: synthetic_memory_probe(total_gib, available_gib),
        paths: Vec::new(),
        tools: Vec::new(),
        environment: synthetic_environment_probe(),
        degraded: Vec::new(),
    }
}

fn profile_artifact(profile: OperatingProfile) -> ArtifactSummary {
    let budgets = ProfileBudgets::for_profile(profile);
    let mut artifact = ArtifactSummary::new(
        format!("profile-{}", profile.as_str()),
        ArtifactKind::ProfileEvidence,
        "ee.profile.runtime.v1",
    )
    .with_profile(ProfileReference {
        profile_name: profile.as_str().to_owned(),
        confidence: Some("high".to_owned()),
        override_source: None,
    })
    .with_command_family("profile");

    artifact.add_metric(
        "profile.budgets.search_candidate_limit",
        MetricValue::measured(budgets.search.candidate_limit as f64, "count"),
    );
    artifact.add_metric(
        "profile.budgets.pack_max_tokens",
        MetricValue::measured(budgets.pack.max_tokens as f64, "tokens"),
    );
    artifact.add_metric(
        "profile.budgets.cache_memory_cap_mb",
        MetricValue::measured(budgets.cache.memory_cap_mb as f64, "mb"),
    );
    artifact.add_metric(
        "profile.budgets.write_spool_batch_cap",
        MetricValue::measured(budgets.write_spool.batch_cap as f64, "records"),
    );

    artifact
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn profile_recommendation_is_deterministic(
        logical_cores in 1_u32..=256,
        total_gib in 1_u64..=512,
        available_ratio in 10_u64..=100,
    ) {
        let available_gib = (total_gib * available_ratio) / 100;
        let probe = synthetic_probe(logical_cores, total_gib, available_gib);

        let profile1 = recommend_operating_profile(&probe);
        let profile2 = recommend_operating_profile(&probe);

        prop_assert_eq!(
            profile1.recommended, profile2.recommended,
            "same inputs must yield same profile"
        );
        prop_assert_eq!(
            profile1.confidence, profile2.confidence,
            "same inputs must yield same confidence"
        );
    }

    #[test]
    fn profile_tiers_are_ordered_by_resources(
        base_cores in 1_u32..=16,
        base_gib in 1_u64..=8,
    ) {
        // Constrained: 1-4 cores, 1-4 GiB
        let constrained = recommend_operating_profile(&synthetic_probe(
            base_cores.min(4),
            base_gib.min(4),
            base_gib.min(4),
        ));

        // Portable: 4-8 cores, 8-16 GiB
        let portable = recommend_operating_profile(&synthetic_probe(
            (base_cores * 2).clamp(4, 8),
            (base_gib * 4).clamp(8, 16),
            (base_gib * 4).clamp(8, 16),
        ));

        // Workstation: 8-32 cores, 16-64 GiB
        let workstation = recommend_operating_profile(&synthetic_probe(
            (base_cores * 4).clamp(8, 32),
            (base_gib * 8).clamp(16, 64),
            (base_gib * 8).clamp(16, 64),
        ));

        // Swarm: 64+ cores, 128+ GiB
        let swarm = recommend_operating_profile(&synthetic_probe(
            (base_cores * 16).max(64),
            (base_gib * 32).max(128),
            (base_gib * 32).max(128),
        ));

        // Verify ordering: constrained <= portable <= workstation <= swarm
        let tier = |p: OperatingProfile| match p {
            OperatingProfile::Constrained => 0,
            OperatingProfile::Portable => 1,
            OperatingProfile::Workstation => 2,
            OperatingProfile::Swarm => 3,
        };

        prop_assert!(
            tier(constrained.recommended) <= tier(portable.recommended),
            "constrained resources should not recommend higher tier than portable"
        );
        prop_assert!(
            tier(portable.recommended) <= tier(workstation.recommended),
            "portable resources should not recommend higher tier than workstation"
        );
        prop_assert!(
            tier(workstation.recommended) <= tier(swarm.recommended),
            "workstation resources should not recommend higher tier than swarm"
        );
    }

    #[test]
    fn budget_scaling_is_monotonic_with_profile_tier(seed in any::<u64>()) {
        let _ = seed; // Consume seed to satisfy proptest

        let constrained = ProfileBudgets::for_profile(OperatingProfile::Constrained);
        let portable = ProfileBudgets::for_profile(OperatingProfile::Portable);
        let workstation = ProfileBudgets::for_profile(OperatingProfile::Workstation);
        let swarm = ProfileBudgets::for_profile(OperatingProfile::Swarm);

        macro_rules! assert_monotonic_budget {
            ($constrained:expr, $portable:expr, $workstation:expr, $swarm:expr, $field:literal) => {{
                prop_assert!(
                    $constrained <= $portable,
                    "{} must be monotonic from constrained to portable",
                    $field
                );
                prop_assert!(
                    $portable <= $workstation,
                    "{} must be monotonic from portable to workstation",
                    $field
                );
                prop_assert!(
                    $workstation <= $swarm,
                    "{} must be monotonic from workstation to swarm",
                    $field
                );
            }};
        }

        assert_monotonic_budget!(
            constrained.search.candidate_limit,
            portable.search.candidate_limit,
            workstation.search.candidate_limit,
            swarm.search.candidate_limit,
            "search.candidate_limit"
        );
        assert_monotonic_budget!(
            constrained.search.concurrent_index_readers,
            portable.search.concurrent_index_readers,
            workstation.search.concurrent_index_readers,
            swarm.search.concurrent_index_readers,
            "search.concurrent_index_readers"
        );
        assert_monotonic_budget!(
            constrained.pack.max_tokens,
            portable.pack.max_tokens,
            workstation.pack.max_tokens,
            swarm.pack.max_tokens,
            "pack.max_tokens"
        );
        assert_monotonic_budget!(
            constrained.pack.max_candidate_memories,
            portable.pack.max_candidate_memories,
            workstation.pack.max_candidate_memories,
            swarm.pack.max_candidate_memories,
            "pack.max_candidate_memories"
        );
        assert_monotonic_budget!(
            constrained.cache.memory_cap_mb,
            portable.cache.memory_cap_mb,
            workstation.cache.memory_cap_mb,
            swarm.cache.memory_cap_mb,
            "cache.memory_cap_mb"
        );
        assert_monotonic_budget!(
            constrained.cache.entry_cap,
            portable.cache.entry_cap,
            workstation.cache.entry_cap,
            swarm.cache.entry_cap,
            "cache.entry_cap"
        );
        assert_monotonic_budget!(
            constrained.cache.hotset_prewarm_limit,
            portable.cache.hotset_prewarm_limit,
            workstation.cache.hotset_prewarm_limit,
            swarm.cache.hotset_prewarm_limit,
            "cache.hotset_prewarm_limit"
        );
        assert_monotonic_budget!(
            constrained.write_spool.queue_cap,
            portable.write_spool.queue_cap,
            workstation.write_spool.queue_cap,
            swarm.write_spool.queue_cap,
            "write_spool.queue_cap"
        );
        assert_monotonic_budget!(
            constrained.write_spool.batch_cap,
            portable.write_spool.batch_cap,
            workstation.write_spool.batch_cap,
            swarm.write_spool.batch_cap,
            "write_spool.batch_cap"
        );
        assert_monotonic_budget!(
            constrained.write_spool.retry_budget,
            portable.write_spool.retry_budget,
            workstation.write_spool.retry_budget,
            swarm.write_spool.retry_budget,
            "write_spool.retry_budget"
        );
        assert_monotonic_budget!(
            constrained.steward.maintenance_window_ms,
            portable.steward.maintenance_window_ms,
            workstation.steward.maintenance_window_ms,
            swarm.steward.maintenance_window_ms,
            "steward.maintenance_window_ms"
        );
        assert_monotonic_budget!(
            constrained.steward.graph_refresh_budget,
            portable.steward.graph_refresh_budget,
            workstation.steward.graph_refresh_budget,
            swarm.steward.graph_refresh_budget,
            "steward.graph_refresh_budget"
        );
    }

    #[test]
    fn minimal_resources_defaults_to_constrained(
        logical_cores in 1_u32..=2,
        total_gib in 1_u64..=2,
    ) {
        let probe = synthetic_probe(logical_cores, total_gib, total_gib);
        let result = recommend_operating_profile(&probe);

        // With minimal resources, should default to constrained
        prop_assert_eq!(
            result.recommended,
            OperatingProfile::Constrained,
            "minimal resources should default to constrained"
        );
    }
}

#[test]
fn profile_budget_conformance_passes_for_all_profiles() {
    for profile in [
        OperatingProfile::Constrained,
        OperatingProfile::Portable,
        OperatingProfile::Workstation,
        OperatingProfile::Swarm,
    ] {
        let artifact = profile_artifact(profile);
        let report = check_profile_budget_artifact_conformance(
            Some(profile),
            profile,
            &[],
            &artifact,
            Some(ProfileBudgets::for_profile(profile).verification.recipe),
        );

        assert_eq!(report.schema, PROFILE_BUDGET_CONFORMANCE_SCHEMA_V1);
        assert!(report.side_effect_free);
        assert_eq!(report.status, ProfileBudgetConformanceStatus::Passed);
        assert!(report.degraded.is_empty(), "{profile:?} should pass");
        assert_eq!(report.checks.len(), 5);
    }
}

#[test]
fn profile_budget_conformance_reports_candidate_limit_too_high() {
    let profile = OperatingProfile::Portable;
    let mut artifact = profile_artifact(profile);
    artifact.add_metric(
        "profile.budgets.search_candidate_limit",
        MetricValue::measured(
            (ProfileBudgets::for_profile(profile).search.candidate_limit + 1) as f64,
            "count",
        ),
    );

    let report = check_profile_budget_artifact_conformance(
        Some(profile),
        profile,
        &[],
        &artifact,
        Some(ProfileBudgets::for_profile(profile).verification.recipe),
    );

    assert_eq!(report.status, ProfileBudgetConformanceStatus::Failed);
    assert!(report.degraded.iter().any(|d| {
        d.code == "observed_budget_above_profile"
            && d.owner == "search"
            && d.field == "profile.budgets.search_candidate_limit"
    }));
}

#[test]
fn profile_budget_conformance_reports_pack_token_cap_too_low() {
    let profile = OperatingProfile::Workstation;
    let mut artifact = profile_artifact(profile);
    artifact.add_metric(
        "profile.budgets.pack_max_tokens",
        MetricValue::measured(
            (ProfileBudgets::for_profile(profile).pack.max_tokens - 1) as f64,
            "tokens",
        ),
    );

    let report = check_profile_budget_artifact_conformance(
        Some(profile),
        profile,
        &[],
        &artifact,
        Some(ProfileBudgets::for_profile(profile).verification.recipe),
    );

    assert_eq!(report.status, ProfileBudgetConformanceStatus::Failed);
    assert!(report.degraded.iter().any(|d| {
        d.code == "observed_budget_below_profile"
            && d.owner == "pack"
            && d.field == "profile.budgets.pack_max_tokens"
    }));
}

#[test]
fn profile_budget_conformance_reports_cache_write_and_recipe_mismatches() {
    let profile = OperatingProfile::Swarm;
    let mut artifact = profile_artifact(profile);
    artifact.add_metric(
        "profile.budgets.cache_memory_cap_mb",
        MetricValue::measured(1.0, "mb"),
    );
    artifact.add_metric(
        "profile.budgets.write_spool_batch_cap",
        MetricValue::measured(1.0, "records"),
    );

    let report = check_profile_budget_artifact_conformance(
        Some(profile),
        profile,
        &[],
        &artifact,
        Some("quick"),
    );

    assert_eq!(report.status, ProfileBudgetConformanceStatus::Failed);
    for (owner, field) in [
        ("cache", "profile.budgets.cache_memory_cap_mb"),
        ("write_spool", "profile.budgets.write_spool_batch_cap"),
        ("profile", "profile.budgets.verification_recipe"),
    ] {
        assert!(
            report
                .degraded
                .iter()
                .any(|d| d.owner == owner && d.field == field),
            "missing conformance degradation for {field}"
        );
    }
}

#[test]
fn profile_budget_conformance_distinguishes_explicit_overrides() {
    let profile = OperatingProfile::Constrained;
    let mut artifact = profile_artifact(profile);
    artifact.add_metric(
        "profile.budgets.search_candidate_limit",
        MetricValue::measured(999.0, "count"),
    );
    let overrides = vec!["profile.budgets.search_candidate_limit".to_owned()];

    let report = check_profile_budget_artifact_conformance(
        Some(profile),
        profile,
        &overrides,
        &artifact,
        Some(ProfileBudgets::for_profile(profile).verification.recipe),
    );

    assert_eq!(report.status, ProfileBudgetConformanceStatus::Degraded);
    assert!(report.degraded.iter().any(|d| {
        d.code == "explicit_override_observed"
            && d.owner == "search"
            && d.field == "profile.budgets.search_candidate_limit"
    }));
}

#[test]
fn profile_budget_conformance_reports_missing_profile_provenance() {
    let profile = OperatingProfile::Portable;
    let mut artifact = profile_artifact(profile);
    artifact.profile = None;
    artifact.add_degradation(SummaryDegradation::missing_metric(
        "profile.budgets.cache_memory_cap_mb",
        Some(&artifact.artifact_id),
    ));
    artifact
        .metrics
        .remove("profile.budgets.cache_memory_cap_mb");

    let report = check_profile_budget_artifact_conformance(
        Some(profile),
        profile,
        &[],
        &artifact,
        Some(ProfileBudgets::for_profile(profile).verification.recipe),
    );

    assert_eq!(report.status, ProfileBudgetConformanceStatus::Degraded);
    assert!(
        report
            .degraded
            .iter()
            .any(|d| { d.code == "profile_provenance_missing" && d.field == "artifact.profile" })
    );
    assert!(report.degraded.iter().any(|d| {
        d.code == "observed_budget_missing" && d.field == "profile.budgets.cache_memory_cap_mb"
    }));
}

#[test]
fn profile_budgets_are_consistent_across_all_profiles() {
    for profile in [
        OperatingProfile::Constrained,
        OperatingProfile::Portable,
        OperatingProfile::Workstation,
        OperatingProfile::Swarm,
    ] {
        let budgets = ProfileBudgets::for_profile(profile);

        // Verify all budgets are positive
        assert!(
            budgets.search.candidate_limit > 0,
            "{profile:?} search.candidate_limit must be positive"
        );
        assert!(
            budgets.pack.max_tokens > 0,
            "{profile:?} pack.max_tokens must be positive"
        );
        assert!(
            budgets.cache.memory_cap_mb > 0,
            "{profile:?} cache.memory_cap_mb must be positive"
        );
        assert!(
            budgets.cache.entry_cap > 0,
            "{profile:?} cache.entry_cap must be positive"
        );
        assert!(
            budgets.write_spool.queue_cap > 0,
            "{profile:?} write_spool.queue_cap must be positive"
        );
        assert!(
            budgets.steward.maintenance_window_ms > 0,
            "{profile:?} steward.maintenance_window_ms must be positive"
        );
    }
}
