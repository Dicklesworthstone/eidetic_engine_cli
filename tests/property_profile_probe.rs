//! Property tests for profile probe → profile recommendation mapping.
//!
//! These tests verify that:
//! 1. Profile recommendation is deterministic for the same probe inputs
//! 2. Profile thresholds partition the resource space correctly
//! 3. Budget scaling is monotonic with profile tier

use ee::core::profile::{
    CpuProbe, EnvironmentProbe, HOST_PROFILE_PROBE_SCHEMA_V1, HostResourceProbeReport, MemoryProbe,
    OperatingProfile, ProfileBudgets, WorkspaceProbe, recommend_operating_profile,
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

        // Search budgets should scale up
        prop_assert!(
            constrained.search.candidate_limit <= portable.search.candidate_limit,
            "search candidate_limit must be monotonic"
        );
        prop_assert!(
            portable.search.candidate_limit <= workstation.search.candidate_limit,
            "search candidate_limit must be monotonic"
        );
        prop_assert!(
            workstation.search.candidate_limit <= swarm.search.candidate_limit,
            "search candidate_limit must be monotonic"
        );

        // Pack budgets should scale up
        prop_assert!(
            constrained.pack.max_tokens <= portable.pack.max_tokens,
            "pack max_tokens must be monotonic"
        );
        prop_assert!(
            portable.pack.max_tokens <= workstation.pack.max_tokens,
            "pack max_tokens must be monotonic"
        );
        prop_assert!(
            workstation.pack.max_tokens <= swarm.pack.max_tokens,
            "pack max_tokens must be monotonic"
        );

        // Cache budgets should scale up
        prop_assert!(
            constrained.cache.memory_cap_mb <= portable.cache.memory_cap_mb,
            "cache memory_cap_mb must be monotonic"
        );
        prop_assert!(
            portable.cache.memory_cap_mb <= workstation.cache.memory_cap_mb,
            "cache memory_cap_mb must be monotonic"
        );
        prop_assert!(
            workstation.cache.memory_cap_mb <= swarm.cache.memory_cap_mb,
            "cache memory_cap_mb must be monotonic"
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
