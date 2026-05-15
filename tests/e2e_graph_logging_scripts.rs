use std::fs;
use std::path::Path;

const GRAPH_E2E_SCRIPTS: &[(&str, &str)] = &[
    ("scripts/e2e_overhaul/g1_ppr.sh", "g1_ppr"),
    ("scripts/e2e_overhaul/g2_pack_dna.sh", "g2_pack_dna"),
    ("scripts/e2e_overhaul/g3_causal.sh", "g3_causal"),
    (
        "scripts/e2e_overhaul/g4_health_structural.sh",
        "g4_health_structural",
    ),
    ("scripts/e2e_overhaul/g5_curate_decay.sh", "g5_curate_decay"),
    ("scripts/e2e_overhaul/g6_proximity.sh", "g6_proximity"),
    (
        "scripts/e2e_overhaul/g7_revision_impact.sh",
        "g7_revision_impact",
    ),
    ("scripts/e2e_overhaul/g8_skyline.sh", "g8_skyline"),
    ("scripts/e2e_overhaul/g9_load_bearing.sh", "g9_load_bearing"),
    ("scripts/e2e_overhaul/g10_hits.sh", "g10_hits"),
];

#[test]
fn graph_e2e_scripts_follow_structured_logging_contract() {
    for (path, epic_name) in GRAPH_E2E_SCRIPTS {
        let contents = read_script(path);
        assert!(
            contents.starts_with("#!/usr/bin/env bash"),
            "{path} must be directly executable as a bash e2e driver",
        );
        assert!(
            contents.contains("source \"$SCRIPT_DIR/lib/shared.sh\""),
            "{path} must source the shared J1/J3 logging helpers",
        );
        assert!(
            contents.contains(&format!("epic_setup \"{epic_name}\"")),
            "{path} must create its own logged temp workspace",
        );
        assert!(
            contents.contains("e2e_log_note"),
            "{path} must log non-trivial intermediate values",
        );
        assert!(
            contents.contains("e2e_log_assert_eq")
                || contents.contains("e2e_log_assert_num")
                || contents.contains("assert_jq"),
            "{path} must record assertions through the structured logger",
        );
        assert!(
            contents.contains("todo_assert"),
            "{path} must record unavailable future graph surfaces structurally",
        );
        assert!(
            contents.contains("EE_GRAPH_E2E_INJECT_FAILURE"),
            "{path} must support deliberate failure injection",
        );
        assert!(
            contents.contains("_injected_failure_diff"),
            "{path} must label the injected expected/actual diff",
        );
        assert!(
            contents.contains("snapshot_version="),
            "{path} must include the exercised snapshot version in its summary",
        );
        assert!(
            contents.contains("elapsed_ms="),
            "{path} must include total elapsed time in its summary",
        );
        assert!(
            contents.contains("EE_TEST_LOG_ASSERTS_PASS")
                && contents.contains("EE_TEST_LOG_ASSERTS_FAIL"),
            "{path} must summarize structured assertion counters",
        );
    }
}

#[test]
fn graph_e2e_script_inventory_matches_bead_coverage() {
    let actual = GRAPH_E2E_SCRIPTS
        .iter()
        .filter(|(path, _)| Path::new(path).exists())
        .count();
    assert_eq!(actual, 10, "bd-8jvg.4 requires ten graph e2e drivers");
}

fn read_script(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}
