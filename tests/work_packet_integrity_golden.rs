//! Golden contract: malformed-tail Beads integrity report (bd-2z5ly.9).
//!
//! The work-packet `Beads integrity` section reports a bounded,
//! deterministic [`BeadsIntegrityReport`] when `.beads/issues.jsonl`
//! contains a malformed final row. The bead requires:
//!
//! 1. Health classified as `jsonl_parse_error`.
//! 2. Recovery hint pointing the agent at `br doctor --json` before
//!    any `br update` / `br claim`.
//! 3. `requiresCandidateDowngrade = true` so the packet refuses to
//!    auto-claim work against a non-authoritative tracker.
//! 4. Byte-stable JSON serialization across runs.
//!
//! This test pins the JSON shape against
//! `tests/fixtures/swarm_work_packet/integrity/malformed_jsonl_tail.json`
//! so any schema or recovery-hint regression fails loudly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use ee::core::beads_integrity::{
    BeadsIntegrityHealth, BeadsIntegrityInputs, BeadsIntegrityRepairClassification,
    JsonlParseError, compose_integrity_report,
};

type TestResult = Result<(), String>;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/swarm_work_packet/integrity/malformed_jsonl_tail.json")
}

fn read_fixture() -> Result<serde_json::Value, String> {
    let path = fixture_path();
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("read fixture {}: {error}", path.display()))?;
    serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|error| format!("parse fixture {}: {error}", path.display()))
}

fn malformed_tail_inputs() -> BeadsIntegrityInputs<'static> {
    BeadsIntegrityInputs {
        jsonl_path: ".beads/issues.jsonl",
        db_path: ".beads/beads.db",
        jsonl_record_count: 2702,
        db_record_count: 2702,
        auto_import_enabled: true,
        external_changes_pending_import: false,
        dirty_issue_count: 0,
        merge_artifact_paths: &[],
        jsonl_parse_error: Some(JsonlParseError {
            line: 2703,
            column: None,
            excerpt: "{\"id\":\"bd-malformed-tail\",\"title\":\"WIP - record was truncated mid"
                .to_owned(),
        }),
    }
}

#[test]
fn malformed_tail_matches_golden_json() -> TestResult {
    let report = compose_integrity_report(malformed_tail_inputs());
    let actual =
        serde_json::to_value(&report).map_err(|error| format!("serialize report: {error}"))?;
    let expected = read_fixture()?;
    if actual != expected {
        let pretty_actual =
            serde_json::to_string_pretty(&actual).unwrap_or_else(|_| "<unserializable>".to_owned());
        let pretty_expected = serde_json::to_string_pretty(&expected)
            .unwrap_or_else(|_| "<unserializable>".to_owned());
        return Err(format!(
            "integrity report JSON drift\n--- expected ---\n{pretty_expected}\n--- actual ---\n{pretty_actual}",
        ));
    }
    Ok(())
}

#[test]
fn malformed_tail_classifies_as_jsonl_parse_error() -> TestResult {
    let report = compose_integrity_report(malformed_tail_inputs());
    if report.health != BeadsIntegrityHealth::JsonlParseError {
        return Err(format!("expected JsonlParseError, got {:?}", report.health));
    }
    if !report.requires_candidate_downgrade {
        return Err("malformed-tail must force candidate downgrade".into());
    }
    if report.recovery_hint.is_none() {
        return Err("malformed-tail must carry a recovery hint".into());
    }
    if report.invalid_line_numbers != vec![2703] {
        return Err(format!(
            "malformed-tail invalid line numbers drifted: {:?}",
            report.invalid_line_numbers
        ));
    }
    if report.safe_repair_candidate != Some(true) {
        return Err("malformed-tail with DB/count evidence should be a repair candidate".into());
    }
    if report.repair_command_candidate != Some("br sync --flush-only --force --json") {
        return Err(format!(
            "malformed-tail repair command drifted: {:?}",
            report.repair_command_candidate
        ));
    }
    if report.repair_classification
        != Some(BeadsIntegrityRepairClassification::InvalidTrailingLineDbHealthy)
    {
        return Err("malformed-tail repair classification drifted".into());
    }
    if report.mutation_must_stop != Some(true) {
        return Err("malformed-tail must stop tracker mutation until repair".into());
    }
    Ok(())
}

#[test]
fn malformed_tail_recovery_hint_blocks_auto_claim() -> TestResult {
    let report = compose_integrity_report(malformed_tail_inputs());
    let hint = report
        .recovery_hint
        .ok_or_else(|| "expected recovery hint".to_owned())?;
    // The hint must direct the agent to inspect before any br update /
    // br claim. Without those instructions the packet could be read as
    // auto-claim-friendly even when the tracker is non-authoritative.
    if !hint.contains("br doctor --json") {
        return Err(format!(
            "recovery hint missing `br doctor --json` directive: {hint}",
        ));
    }
    if !(hint.contains("br update") || hint.contains("br claim")) {
        return Err(format!(
            "recovery hint must mention br update / br claim before continuing: {hint}",
        ));
    }
    Ok(())
}

#[test]
fn serialization_is_byte_stable_across_runs() -> TestResult {
    let inputs = malformed_tail_inputs();
    let first = compose_integrity_report(inputs.clone());
    let second = compose_integrity_report(inputs);
    let first_json =
        serde_json::to_string(&first).map_err(|error| format!("serialize first: {error}"))?;
    let second_json =
        serde_json::to_string(&second).map_err(|error| format!("serialize second: {error}"))?;
    if first_json != second_json {
        return Err(format!(
            "byte-stability broken:\n  first  = {first_json}\n  second = {second_json}",
        ));
    }
    Ok(())
}
