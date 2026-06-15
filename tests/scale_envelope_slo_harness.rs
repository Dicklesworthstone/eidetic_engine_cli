//! bd-ssoco.5: deterministic scale-envelope SLO harness.
//!
//! The small profile is cheap enough for ordinary test gates. The full profile
//! is intentionally an ignored RCH-only gate so local Cargo never has to
//! materialize or prove million-memory scale on this Mac.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use ee::core::lab::{
    SCALE_ENVELOPE_FIXTURE_MANIFEST_SCHEMA_V1, SCALE_ENVELOPE_SLO_HARNESS_SCHEMA_V1,
    ScaleEnvelopeFixtureOptions, ScaleEnvelopeFixtureProfile, ScaleEnvelopeSloCommandObservation,
    ScaleEnvelopeSloHarnessOptions, ScaleEnvelopeSloHarnessStatus, ScaleEnvelopeSloLatencyMs,
    ScaleEnvelopeSloProofLane, generate_scale_envelope_fixture_manifest,
    generate_scale_envelope_slo_harness_report,
};
use ee::models::{
    SCALE_ENVELOPE_SCHEMA_V1, SCALE_POSTURE_THRASHING_CODE, SCALE_POSTURE_WARMING_CODE,
    SCALE_PROBE_BUDGET_EXCEEDED_CODE, TEST_EVENT_SCHEMA_V1,
};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const SMALL_SEED: &str = "scale_slo_small_seed_001";
const FULL_SEED: &str = "scale_slo_full_seed_001";

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn hash(label: &str) -> String {
    format!("blake3:{}", blake3::hash(label.as_bytes()).to_hex())
}

fn assert_prefixed_hash(value: &str, context: &str) -> TestResult {
    ensure(
        value.starts_with("blake3:") && value.len() == "blake3:".len() + 64,
        format!("{context} should be a BLAKE3 hash, got {value}"),
    )
}

fn full_rch_observations() -> Vec<ScaleEnvelopeSloCommandObservation> {
    [
        ("ingest", 4_000, 7_500, 11_000, 384 * 1024 * 1024, 800),
        ("search", 3_200, 6_200, 9_200, 320 * 1024 * 1024, 700),
        ("pack", 4_800, 8_400, 12_400, 448 * 1024 * 1024, 900),
        ("maintain", 5_200, 9_100, 13_100, 512 * 1024 * 1024, 950),
    ]
    .into_iter()
    .map(
        |(surface, p50, p95, p99, rss_estimate_bytes, page_faults_delta)| {
            ScaleEnvelopeSloCommandObservation::rch_evidence(
                surface,
                vec![
                    "ee".to_owned(),
                    surface.to_owned(),
                    "--workspace".to_owned(),
                    ".".to_owned(),
                    "--json".to_owned(),
                ],
                33,
                ScaleEnvelopeSloLatencyMs { p50, p95, p99 },
                rss_estimate_bytes,
                page_faults_delta,
                hash(&format!("full-rch-{surface}")),
            )
        },
    )
    .collect()
}

#[test]
fn small_profile_contract_and_golden_summary_are_stable() -> TestResult {
    let first = generate_scale_envelope_slo_harness_report(&ScaleEnvelopeSloHarnessOptions::small(
        SMALL_SEED,
    ));
    let second = generate_scale_envelope_slo_harness_report(
        &ScaleEnvelopeSloHarnessOptions::small(SMALL_SEED),
    );
    ensure(
        first == second,
        "same seed should generate equal small-profile harness reports",
    )?;
    ensure(
        first.to_json_pretty() == second.to_json_pretty(),
        "small-profile report should be byte-stable in pretty JSON",
    )?;
    ensure(
        first.schema == SCALE_ENVELOPE_SLO_HARNESS_SCHEMA_V1
            && first.envelope_schema == SCALE_ENVELOPE_SCHEMA_V1
            && first.profile == ScaleEnvelopeFixtureProfile::Small
            && first.status == ScaleEnvelopeSloHarnessStatus::Pass,
        "small harness should pass while targeting ee.scale_envelope.v1",
    )?;
    ensure(
        first.small_profile_gate == "ordinary_test_contract_and_golden"
            && first.full_profile_gate == "rch_only_ingest_search_pack_maintain",
        "small and full profile gates should be explicit",
    )?;
    ensure(
        !first.local_cargo_allowed_for_full_profile && !first.local_large_corpus_generation_allowed,
        "full profile must never require local Cargo or local large-corpus generation",
    )?;
    ensure(
        first.command_slos.len() == 4
            && first
                .command_slos
                .iter()
                .all(|observation| observation.evidence.event_schema == TEST_EVENT_SCHEMA_V1),
        "small profile should record all command SLO surfaces with test-event provenance",
    )?;

    let degraded_codes = first
        .representative_degraded_states
        .iter()
        .map(|state| state.degraded_code.as_str())
        .collect::<BTreeSet<_>>();
    ensure(
        degraded_codes
            == BTreeSet::from([
                SCALE_POSTURE_WARMING_CODE,
                SCALE_POSTURE_THRASHING_CODE,
                SCALE_PROBE_BUDGET_EXCEEDED_CODE,
            ]),
        "small profile should pin representative degraded states",
    )?;
    ensure(
        first.event_log_rows.iter().all(|row| {
            row.schema == TEST_EVENT_SCHEMA_V1
                && row.test_id == "bd-ssoco.5.scale_envelope_slo"
                && !row.fields.is_empty()
        }),
        "event rows should be ee.test_event.v1 support-bundle evidence",
    )?;
    assert_prefixed_hash(&first.hash_summary.report_hash, "report_hash")?;
    assert_prefixed_hash(&first.hash_summary.command_slos_hash, "command_slos_hash")?;
    assert_prefixed_hash(&first.hash_summary.event_log_hash, "event_log_hash")?;

    let golden_summary = json!({
        "schema": first.schema,
        "targetSchema": first.envelope_schema,
        "profile": first.profile,
        "status": first.status,
        "surfaces": first
            .command_slos
            .iter()
            .map(|observation| {
                json!({
                    "surface": observation.surface,
                    "p50": observation.latency_ms.p50,
                    "p95": observation.latency_ms.p95,
                    "p99": observation.latency_ms.p99,
                    "checkpoint": observation.checkpoint_posture,
                    "index": observation.index_posture,
                    "substrate": observation.evidence.substrate,
                })
            })
            .collect::<Vec<Value>>(),
    });
    ensure(
        golden_summary
            == json!({
                "schema": SCALE_ENVELOPE_SLO_HARNESS_SCHEMA_V1,
                "targetSchema": SCALE_ENVELOPE_SCHEMA_V1,
                "profile": "small",
                "status": "pass",
                "surfaces": [
                    {"surface": "ingest", "p50": 21, "p95": 46, "p99": 77, "checkpoint": "clean", "index": "fresh", "substrate": "ordinary_test"},
                    {"surface": "search", "p50": 24, "p95": 50, "p99": 82, "checkpoint": "clean", "index": "fresh", "substrate": "ordinary_test"},
                    {"surface": "pack", "p50": 27, "p95": 54, "p99": 87, "checkpoint": "clean", "index": "fresh", "substrate": "ordinary_test"},
                    {"surface": "maintain", "p50": 30, "p95": 58, "p99": 92, "checkpoint": "clean", "index": "fresh", "substrate": "ordinary_test"}
                ]
            }),
        "small-profile golden summary drifted",
    )?;

    let parsed: Value =
        serde_json::from_str(&first.to_json()).map_err(|error| error.to_string())?;
    ensure(
        parsed.pointer("/schema").and_then(Value::as_str)
            == Some(SCALE_ENVELOPE_SLO_HARNESS_SCHEMA_V1)
            && parsed.pointer("/envelopeSchema").and_then(Value::as_str)
                == Some(SCALE_ENVELOPE_SCHEMA_V1),
        "serialized report should preserve harness and target schemas",
    )
}

#[test]
fn full_profile_plan_is_rch_only_and_fails_closed_without_evidence() -> TestResult {
    let manifest =
        generate_scale_envelope_fixture_manifest(&ScaleEnvelopeFixtureOptions::full(FULL_SEED));
    ensure(
        manifest.schema == SCALE_ENVELOPE_FIXTURE_MANIFEST_SCHEMA_V1
            && !manifest.output_policy.materialized_in_ci
            && manifest.output_policy.rch_required_for_full_materialization,
        "full fixture manifest should stay RCH-oriented",
    )?;

    let blocked = generate_scale_envelope_slo_harness_report(
        &ScaleEnvelopeSloHarnessOptions::full_rch(FULL_SEED),
    );
    ensure(
        blocked.status == ScaleEnvelopeSloHarnessStatus::Blocked
            && blocked.command_slos.is_empty()
            && blocked
                .degraded_codes
                .contains(&SCALE_PROBE_BUDGET_EXCEEDED_CODE.to_owned()),
        "full profile should block instead of fabricating missing SLO evidence",
    )?;
    ensure(
        blocked.missing_evidence.len() == 4
            && blocked
                .missing_evidence
                .iter()
                .all(|entry| entry.contains(TEST_EVENT_SCHEMA_V1)),
        "every full-profile surface should require ee.test_event.v1 evidence",
    )?;
    ensure(
        blocked.rch_command.first().map(String::as_str) == Some("scripts/rch_verify.sh")
            && blocked.rch_command.iter().any(|arg| arg == "--test")
            && blocked
                .rch_command
                .iter()
                .any(|arg| arg == "scale_envelope_slo_harness"),
        "full-profile proof command should route through scripts/rch_verify.sh",
    )?;
    ensure(
        !blocked.local_cargo_allowed_for_full_profile
            && !blocked
                .regression_policy
                .local_cargo_allowed_for_full_profile
            && blocked.regression_policy.fail_closed_when_evidence_missing,
        "regression policy should fail closed and disallow local full-profile Cargo",
    )
}

#[test]
#[ignore = "full-profile SLO gate is RCH-only; invoke through scripts/rch_verify.sh"]
fn full_profile_rch_slo_gate() -> TestResult {
    let report = generate_scale_envelope_slo_harness_report(
        &ScaleEnvelopeSloHarnessOptions::full_rch(FULL_SEED)
            .with_observations(full_rch_observations()),
    );
    ensure(
        report.profile == ScaleEnvelopeFixtureProfile::Full
            && report.proof_lane == ScaleEnvelopeSloProofLane::RchOnlyFullProfile
            && report.status == ScaleEnvelopeSloHarnessStatus::Pass
            && report.missing_evidence.is_empty(),
        "full profile should pass only when all RCH evidence is present",
    )?;
    ensure(
        report.command_slos.iter().all(|observation| {
            observation.evidence.substrate == "rch_remote"
                && observation.evidence.event_schema == TEST_EVENT_SCHEMA_V1
                && observation.latency_ms.p50 > 0
                && observation.latency_ms.p95 >= observation.latency_ms.p50
                && observation.latency_ms.p99 >= observation.latency_ms.p95
                && observation.output_hash.starts_with("blake3:")
                && observation
                    .evidence
                    .support_bundle_ref
                    .contains(".ee/support/scale-envelope")
        }),
        "full-profile observations should carry RCH metrics and support-bundle provenance",
    )?;
    ensure(
        report.event_log_rows.iter().any(|row| {
            row.kind == "artifact_manifest"
                && row
                    .fields
                    .get("support_bundle_ref")
                    .is_some_and(|value| value == ".ee/support/scale-envelope")
        }),
        "event log should include support-bundle provenance",
    )?;
    assert_prefixed_hash(
        &report.hash_summary.fixture_manifest_hash,
        "fixture_manifest_hash",
    )?;
    assert_prefixed_hash(&report.hash_summary.thresholds_hash, "thresholds_hash")?;
    assert_prefixed_hash(&report.hash_summary.command_slos_hash, "command_slos_hash")?;
    assert_prefixed_hash(&report.hash_summary.report_hash, "report_hash")
}
