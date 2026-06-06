//! Contract for the H3.4 calibration freshness / degradation / repair-action
//! deliverable (bd-1zb7k.12.3.4).
//!
//! Pins the conservative posture guarantees agents rely on when consuming
//! `ee.host_calibration.host_class.v1` reports:
//!
//! - every `HostCalibrationFreshness` state outside `Fresh` emits one of the
//!   seven closed-set `host_calibration_*` degraded codes the taxonomy lists,
//! - RCH-only topology is reported as a calibration-confidence blocker rather
//!   than as proof that the local host is weak,
//! - repair actions and degradation `repair` messages stay strictly
//!   non-destructive — no `rm`, `git reset`, `git clean`, `--force`, `--hard`,
//!   or branch deletion appears in either field, and
//! - the report is byte-equal across repeated calls for the same probe and
//!   options (clock-independent / order-independent).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{collections::BTreeSet, fs};

use ee::core::profile::{
    CpuProbe, EnvironmentProbe, HOST_PROFILE_PROBE_SCHEMA_V1, HostCalibrationFreshness,
    HostClassReport, HostClassificationOptions, HostResourceProbeReport, HostTopologyProbe,
    MemoryProbe, OperatingProfile, PathCapacityProbe, RchTopologyProbe, WorkspaceProbe,
    classify_host_profile,
};
use serde_json::json;

const GIB: u64 = 1024 * 1024 * 1024;

const ALL_FRESHNESS_STATES: &[HostCalibrationFreshness] = &[
    HostCalibrationFreshness::Fresh,
    HostCalibrationFreshness::Stale,
    HostCalibrationFreshness::Partial,
    HostCalibrationFreshness::SyntheticOnly,
    HostCalibrationFreshness::Contradictory,
    HostCalibrationFreshness::Missing,
    HostCalibrationFreshness::Unavailable,
];

const CALIBRATION_DEGRADED_CODES: &[&str] = &[
    "host_calibration_stale",
    "host_calibration_partial",
    "host_calibration_synthetic_only",
    "host_calibration_contradictory",
    "host_calibration_missing",
    "host_calibration_unavailable",
    "host_calibration_rch_topology_blocked",
];

const DESTRUCTIVE_TOKENS: &[&str] = &[
    "rm ",
    "rm -",
    "git reset",
    "git clean",
    "git checkout --",
    "git restore --staged",
    "git branch -D",
    "git stash drop",
    "--force",
    "--hard",
    "drop table",
    "truncate ",
    "delete from",
];

fn synthetic_probe(
    logical_cores: u32,
    total_gib: u64,
    rch_available: bool,
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
            physical_cores: None,
            source: "contract_test",
        },
        memory: MemoryProbe {
            total_bytes: Some(total_gib * GIB),
            available_bytes: Some(total_gib.saturating_mul(GIB).saturating_div(2)),
            cgroup_limit_bytes: None,
            source: "contract_test",
        },
        paths: Vec::new(),
        tools: Vec::new(),
        environment: EnvironmentProbe {
            tmpdir_configured: true,
            cargo_target_dir_configured: false,
            rch_hint_configured: false,
            redaction: "presence_only",
        },
        topology: HostTopologyProbe {
            rch: if rch_available {
                RchTopologyProbe {
                    available: true,
                    status: "available_not_queried",
                    posture: "ok",
                    source: "contract_test",
                    message: "RCH available for contract probe.".to_string(),
                    repair: None,
                }
            } else {
                RchTopologyProbe {
                    available: false,
                    status: "missing",
                    posture: "degraded_recoverable",
                    source: "contract_test",
                    message: "RCH unavailable for contract probe.".to_string(),
                    repair: Some("Install rch before heavy Cargo verification."),
                }
            },
        },
        degraded: Vec::new(),
    }
}

fn path_capacity(
    label: &'static str,
    role: &'static str,
    available_gib: u64,
    same_filesystem_as_workspace: Option<bool>,
) -> PathCapacityProbe {
    PathCapacityProbe {
        label,
        role,
        path: None,
        exists: true,
        nearest_existing_ancestor: false,
        same_filesystem_as_workspace,
        total_bytes: Some(available_gib * GIB),
        available_bytes: Some(available_gib * GIB),
        redaction: "path_not_emitted",
    }
}

fn synthetic_topology_probe(
    logical_cores: u32,
    total_gib: u64,
    cargo_target_gib: u64,
    cargo_target_external: bool,
    rch_available: bool,
) -> HostResourceProbeReport {
    let mut probe = synthetic_probe(logical_cores, total_gib, rch_available);
    probe.memory.available_bytes = probe.memory.total_bytes;
    probe.environment.cargo_target_dir_configured = cargo_target_external;
    probe.paths = vec![
        path_capacity("workspace", "workspace_root", cargo_target_gib, Some(true)),
        path_capacity(
            "cargo_target",
            "cargo_target_dir",
            cargo_target_gib,
            Some(!cargo_target_external),
        ),
    ];
    probe
}

fn classify_with_freshness(
    probe: &HostResourceProbeReport,
    freshness: HostCalibrationFreshness,
) -> HostClassReport {
    classify_host_profile(
        probe,
        &HostClassificationOptions {
            calibration_freshness: freshness,
            synthetic_fixture_profile: if matches!(
                freshness,
                HostCalibrationFreshness::SyntheticOnly
            ) {
                Some(OperatingProfile::Swarm)
            } else {
                None
            },
        },
    )
}

fn contains_destructive_token(text: &str) -> Option<&'static str> {
    let lowered = text.to_ascii_lowercase();
    DESTRUCTIVE_TOKENS
        .iter()
        .copied()
        .find(|token| lowered.contains(token))
}

#[test]
fn synthetic_host_topology_golden_matrix_is_stable() {
    let cases = [
        ("portable", synthetic_topology_probe(4, 12, 32, false, true)),
        ("laptop", synthetic_topology_probe(4, 12, 32, true, true)),
        (
            "workstation",
            synthetic_topology_probe(8, 32, 64, false, true),
        ),
        (
            "local_256gb",
            synthetic_topology_probe(32, 256, 512, true, true),
        ),
        (
            "rch_only_topology",
            synthetic_topology_probe(16, 64, 4, false, true),
        ),
    ];

    let actual = cases
        .into_iter()
        .map(|(name, probe)| {
            let report = classify_with_freshness(&probe, HostCalibrationFreshness::Fresh);
            json!({
                "case": name,
                "hostClass": report.host_class,
                "profileCeiling": report.profile_ceiling,
                "confidence": report.confidence,
                "reasonCodes": report.reason_codes,
                "repairActionKinds": report
                    .repair_actions
                    .iter()
                    .map(|action| action.kind)
                    .collect::<Vec<_>>(),
                "degradedCodes": report
                    .degraded
                    .iter()
                    .map(|entry| entry.code)
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();

    let expected = json!([
        {
            "case": "portable",
            "hostClass": "portable",
            "profileCeiling": "portable",
            "confidence": "high",
            "reasonCodes": [
                "cpu_logical_cores_portable",
                "memory_available_portable",
                "disk_capacity_sufficient",
                "target_dir_shared",
                "rch_topology_available",
                "calibration_fresh"
            ],
            "repairActionKinds": [],
            "degradedCodes": []
        },
        {
            "case": "laptop",
            "hostClass": "laptop",
            "profileCeiling": "portable",
            "confidence": "high",
            "reasonCodes": [
                "cpu_logical_cores_portable",
                "memory_available_portable",
                "disk_capacity_sufficient",
                "target_dir_external",
                "rch_topology_available",
                "calibration_fresh"
            ],
            "repairActionKinds": [],
            "degradedCodes": []
        },
        {
            "case": "workstation",
            "hostClass": "workstation",
            "profileCeiling": "workstation",
            "confidence": "high",
            "reasonCodes": [
                "cpu_logical_cores_workstation",
                "memory_available_swarm",
                "disk_capacity_sufficient",
                "target_dir_shared",
                "rch_topology_available",
                "calibration_fresh"
            ],
            "repairActionKinds": [],
            "degradedCodes": []
        },
        {
            "case": "local_256gb",
            "hostClass": "local_256gb",
            "profileCeiling": "swarm",
            "confidence": "high",
            "reasonCodes": [
                "cpu_logical_cores_swarm",
                "memory_available_swarm",
                "disk_capacity_swarm_ready",
                "target_dir_external",
                "rch_topology_available",
                "calibration_fresh"
            ],
            "repairActionKinds": [],
            "degradedCodes": []
        },
        {
            "case": "rch_only_topology",
            "hostClass": "rch_only_topology",
            "profileCeiling": "portable",
            "confidence": "high",
            "reasonCodes": [
                "cpu_logical_cores_swarm",
                "memory_available_swarm",
                "disk_capacity_constrained",
                "target_dir_shared",
                "rch_topology_available",
                "calibration_fresh"
            ],
            "repairActionKinds": ["rch_status_probe"],
            "degradedCodes": ["host_calibration_rch_topology_blocked"]
        }
    ]);

    assert_eq!(serde_json::Value::Array(actual.clone()), expected);
    let first = serde_json::to_string(&actual).expect("matrix serializes");
    let second = serde_json::to_string(&expected).expect("expected matrix serializes");
    assert_eq!(
        first, second,
        "host topology matrix JSON must be byte-stable"
    );
}

#[test]
fn host_calibration_e2e_script_logs_decisions_without_destructive_cleanup() {
    let script_path = format!(
        "{}/scripts/e2e_overhaul/host_calibration.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let script = fs::read_to_string(&script_path)
        .unwrap_or_else(|error| panic!("failed to read {script_path}: {error}"));

    for needle in [
        "EE_TEST_LOG_PATH",
        "ee.test_event.v1",
        "kind: \"host_calibration\"",
        "phase: $operation",
        "exit_code: ($rc | tonumber)",
    ] {
        assert!(
            script.contains(needle),
            "host calibration e2e script must log `{needle}` for no-mock decision auditing"
        );
    }

    let lowered = script.to_ascii_lowercase();
    for forbidden in [
        "rm -",
        "git reset",
        "git clean",
        "git checkout --",
        "git worktree",
        "git stash",
        "drop table",
        "truncate ",
        "delete from",
        "ee serve",
    ] {
        assert!(
            !lowered.contains(forbidden),
            "host calibration e2e script must not contain destructive or daemon-only operation `{forbidden}`"
        );
    }
}

#[test]
fn every_unfresh_freshness_state_emits_a_known_calibration_degradation() {
    let probe = synthetic_probe(8, 32, /* rch_available */ true);

    for freshness in ALL_FRESHNESS_STATES
        .iter()
        .copied()
        .filter(|state| !matches!(state, HostCalibrationFreshness::Fresh))
    {
        let report = classify_with_freshness(&probe, freshness);
        let emitted: Vec<&str> = report
            .degraded
            .iter()
            .map(|entry| entry.code)
            .filter(|code| code.starts_with("host_calibration_"))
            .collect();
        assert!(
            !emitted.is_empty(),
            "freshness {:?} must emit at least one host_calibration_* degraded entry, got none",
            freshness
        );
        for code in &emitted {
            assert!(
                CALIBRATION_DEGRADED_CODES.contains(code),
                "freshness {:?} emitted unknown host_calibration_* code {:?}; closed set is {:?}",
                freshness,
                code,
                CALIBRATION_DEGRADED_CODES
            );
        }
    }
}

#[test]
fn fresh_calibration_emits_no_warning_calibration_degradation() {
    let probe = synthetic_probe(8, 32, /* rch_available */ true);
    let report = classify_with_freshness(&probe, HostCalibrationFreshness::Fresh);

    for entry in &report.degraded {
        if entry.severity == "info" {
            continue;
        }
        assert!(
            !entry.code.starts_with("host_calibration_")
                || entry.code == "host_calibration_rch_topology_blocked",
            "fresh calibration with available RCH must not emit warning-severity host_calibration_* entries, got {:?}",
            entry
        );
    }
}

#[test]
fn rch_only_topology_reports_topology_blocker_not_local_weakness() {
    let probe = synthetic_probe(16, 64, /* rch_available */ false);
    let report = classify_with_freshness(&probe, HostCalibrationFreshness::Fresh);

    let codes: BTreeSet<&str> = report.degraded.iter().map(|entry| entry.code).collect();
    assert!(
        codes.contains("host_calibration_rch_topology_blocked"),
        "RCH-only topology must emit host_calibration_rch_topology_blocked, got {:?}",
        codes
    );

    for entry in &report.degraded {
        let lowered = entry.message.to_ascii_lowercase();
        assert!(
            !lowered.contains("weak")
                && !lowered.contains("underpowered")
                && !lowered.contains("insufficient hardware"),
            "RCH-only topology degradation message must not characterise the local host as weak: {:?}",
            entry.message
        );
    }
}

#[test]
fn repair_actions_and_degradation_repairs_are_non_destructive() {
    let probe = synthetic_probe(8, 32, /* rch_available */ false);

    for freshness in ALL_FRESHNESS_STATES.iter().copied() {
        let report = classify_with_freshness(&probe, freshness);

        for action in &report.repair_actions {
            if let Some(command) = action.command {
                assert!(
                    contains_destructive_token(command).is_none(),
                    "freshness {:?} repair_action.command must be non-destructive: {:?}",
                    freshness,
                    command
                );
            }
            assert!(
                contains_destructive_token(action.message).is_none(),
                "freshness {:?} repair_action.message must be non-destructive: {:?}",
                freshness,
                action.message
            );
        }

        for entry in &report.degraded {
            if let Some(repair) = entry.repair {
                assert!(
                    contains_destructive_token(repair).is_none(),
                    "freshness {:?} degraded[{}].repair must be non-destructive: {:?}",
                    freshness,
                    entry.code,
                    repair
                );
            }
            assert!(
                contains_destructive_token(entry.message).is_none(),
                "freshness {:?} degraded[{}].message must be non-destructive: {:?}",
                freshness,
                entry.code,
                entry.message
            );
        }
    }
}

#[test]
fn calibration_freshness_report_is_byte_equal_across_repeat_calls() {
    let probe = synthetic_probe(8, 32, /* rch_available */ false);

    for freshness in ALL_FRESHNESS_STATES.iter().copied() {
        let first = serde_json::to_string(&classify_with_freshness(&probe, freshness))
            .expect("first calibration report must serialize");
        let second = serde_json::to_string(&classify_with_freshness(&probe, freshness))
            .expect("second calibration report must serialize");
        assert_eq!(
            first, second,
            "freshness {:?} must produce byte-equal JSON across repeated calls",
            freshness
        );
    }
}

#[test]
fn unfresh_calibration_caps_profile_ceiling_below_full_workstation() {
    let probe = synthetic_probe(32, 256, /* rch_available */ true);

    let fresh = classify_with_freshness(&probe, HostCalibrationFreshness::Fresh);
    let unfresh_states = [
        HostCalibrationFreshness::Stale,
        HostCalibrationFreshness::Partial,
        HostCalibrationFreshness::SyntheticOnly,
        HostCalibrationFreshness::Contradictory,
        HostCalibrationFreshness::Missing,
        HostCalibrationFreshness::Unavailable,
    ];

    for freshness in unfresh_states {
        let report = classify_with_freshness(&probe, freshness);
        assert!(
            report.profile_ceiling <= fresh.profile_ceiling,
            "freshness {:?} must not raise the profile ceiling above the fresh-calibration ceiling ({:?} > {:?})",
            freshness,
            report.profile_ceiling,
            fresh.profile_ceiling
        );
    }
}
