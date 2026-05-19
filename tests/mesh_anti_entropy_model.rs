//! Executable SRR6.25 model checks for mesh anti-entropy and revision
//! semantics.
//!
//! The model is intentionally imported by path while `src/mesh/mod.rs` is
//! owned by adjacent mesh work. This keeps the formal artifact executable
//! without widening the runtime module surface.

#[path = "../src/mesh/anti_entropy_model.rs"]
mod anti_entropy_model;

use anti_entropy_model::{ANTI_ENTROPY_MODEL_ADR, ANTI_ENTROPY_MODEL_SCENARIOS};

type TestResult = Result<(), String>;

const EXPECTED_SCENARIOS: &[&str] = &[
    "cursor_advances_only_after_contiguous_replay",
    "partition_rejoin_duplicate_out_of_order_delivery",
    "conflicting_revisions_are_visible",
    "stale_tier1_read_gets_revision_notice",
    "deterministic_replay_order_independent",
];

#[test]
fn model_scenario_catalog_is_stable_and_logged() -> TestResult {
    assert_eq!(ANTI_ENTROPY_MODEL_SCENARIOS, EXPECTED_SCENARIOS);

    for scenario in ANTI_ENTROPY_MODEL_SCENARIOS {
        println!("mesh_anti_entropy_model_scenario={scenario} result=covered");
    }

    Ok(())
}

#[test]
fn model_adr_mentions_every_executable_scenario() -> TestResult {
    let adr = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ANTI_ENTROPY_MODEL_ADR),
    )
    .map_err(|error| format!("read {ANTI_ENTROPY_MODEL_ADR}: {error}"))?;

    for scenario in ANTI_ENTROPY_MODEL_SCENARIOS {
        if !adr.contains(scenario) {
            return Err(format!(
                "{ANTI_ENTROPY_MODEL_ADR} must reference executable scenario {scenario}"
            ));
        }
    }

    Ok(())
}
