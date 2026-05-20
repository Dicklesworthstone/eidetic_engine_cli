#![allow(clippy::expect_used)]

#[path = "../src/steward/spec_pack.rs"]
mod spec_pack;

use serde_json::Value;
use spec_pack::{
    RecentQueryObservation, RecentQueryRing, RecentQueryShape,
    SPEC_PACK_CACHE_ADMISSION_DENIED_CODE, SPEC_PACK_QOS_BACK_PRESSURE_CODE,
    SPEC_PACK_RING_EMPTY_CODE, SPEC_PACK_SCHEMA_V1, SpecPackAbortReason, SpecPackAdmissionVerdict,
    SpecPackCacheFreshness, SpecPackCandidate, SpecPackConfig, SpecPackPhase, SpecPackQosSnapshot,
    SpecPackTelemetryEvent, admit_spec_pack_candidate, select_spec_pack_candidates,
};

type TestResult = Result<(), String>;

fn shape(query: &str, bead_id: Option<&str>, max_tokens: u32) -> RecentQueryShape {
    RecentQueryShape::new(
        "workspace-main",
        query,
        bead_id,
        max_tokens,
        "compact",
        Some("CopperPrairie"),
    )
}

fn observation(query: &str, observed_at_ms: u64, tokens_used: u64) -> RecentQueryObservation {
    RecentQueryObservation::new(
        shape(query, Some("bd-20bdb"), 4_000),
        tokens_used,
        observed_at_ms,
    )
}

fn serialized_event(event: &SpecPackTelemetryEvent) -> Result<Value, String> {
    serde_json::to_value(event).map_err(|error| error.to_string())
}

#[test]
fn default_config_clamps_effective_runtime_limits() {
    let defaults = SpecPackConfig::default();
    assert_eq!(defaults.effective_ttl_seconds(), 30);
    assert_eq!(defaults.effective_concurrency(), 4);
    assert_eq!(defaults.effective_ring_capacity(), 64);
    assert_eq!(defaults.effective_top_k(), 4);

    let clamped = SpecPackConfig::new(9_999, 0, 0, 0, 0);
    assert_eq!(clamped.effective_ttl_seconds(), 3_600);
    assert_eq!(clamped.effective_concurrency(), 1);
    assert_eq!(clamped.effective_ring_capacity(), 1);
    assert_eq!(clamped.effective_top_k(), 1);
}

#[test]
fn recent_query_ring_deduplicates_and_evicts_by_count_and_tokens() {
    let mut ring = RecentQueryRing::new(2, 100);
    assert!(ring.is_empty(), "{SPEC_PACK_RING_EMPTY_CODE}");
    ring.record(observation("first", 1, 30));
    ring.record(observation("second", 2, 40));
    ring.record(observation("first", 3, 20));

    let entries = ring.entries_newest_first();
    assert_eq!(ring.len(), 2);
    assert_eq!(ring.total_tokens(), 60);
    assert_eq!(entries[0].shape.query, "first");
    assert_eq!(entries[0].observed_at_ms, 3);
    assert_eq!(entries[1].shape.query, "second");

    ring.record(observation("third", 4, 90));
    let entries = ring.entries_newest_first();
    assert_eq!(ring.len(), 1);
    assert_eq!(ring.total_tokens(), 90);
    assert_eq!(entries[0].shape.query, "third");
}

#[test]
fn select_spec_pack_candidates_skips_fresh_l2_and_sorts_newest_first() {
    let selected = select_spec_pack_candidates(
        [
            SpecPackCandidate::new(observation("fresh", 30, 30), SpecPackCacheFreshness::Fresh),
            SpecPackCandidate::new(
                observation("missing-old", 10, 30),
                SpecPackCacheFreshness::Missing,
            ),
            SpecPackCandidate::new(
                observation("stale-new", 40, 30),
                SpecPackCacheFreshness::Stale,
            ),
            SpecPackCandidate::new(
                observation("missing-mid", 20, 30),
                SpecPackCacheFreshness::Missing,
            ),
        ],
        2,
    );

    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].observed_at_ms, 40);
    assert_eq!(selected[0].cache_freshness, SpecPackCacheFreshness::Stale);
    assert_eq!(selected[1].observed_at_ms, 20);
    assert!(selected.iter().all(|candidate| {
        candidate.workspace_id_hash.starts_with("blake3:")
            && candidate.query_shape_hash.starts_with("blake3:")
    }));
}

#[test]
fn admission_gate_denies_every_foreground_pressure_source_before_speculation() {
    let config = SpecPackConfig::new(30, 2, 64, 8_000, 4);
    assert_eq!(
        admit_spec_pack_candidate(
            &config,
            SpecPackQosSnapshot {
                foreground_request_id_active: true,
                ..SpecPackQosSnapshot::idle()
            },
        ),
        SpecPackAdmissionVerdict::DeniedForegroundRequestIdActive
    );
    assert_eq!(
        admit_spec_pack_candidate(
            &config,
            SpecPackQosSnapshot {
                read_pool_foreground_pin_held: true,
                ..SpecPackQosSnapshot::idle()
            },
        ),
        SpecPackAdmissionVerdict::DeniedReadPoolForegroundPinHeld
    );
    assert_eq!(
        admit_spec_pack_candidate(
            &config,
            SpecPackQosSnapshot {
                qos_foreground_active: true,
                ..SpecPackQosSnapshot::idle()
            },
        ),
        SpecPackAdmissionVerdict::DeniedQosForegroundPressure
    );
    assert_eq!(
        admit_spec_pack_candidate(
            &config,
            SpecPackQosSnapshot {
                active_speculations_for_workspace: 2,
                ..SpecPackQosSnapshot::idle()
            },
        ),
        SpecPackAdmissionVerdict::DeniedPerWorkspaceConcurrencyCap
    );
    assert_eq!(
        admit_spec_pack_candidate(&config, SpecPackQosSnapshot::idle()),
        SpecPackAdmissionVerdict::Admitted
    );
}

#[test]
fn admission_telemetry_is_schema_shaped_hashed_and_side_effect_free() -> TestResult {
    let query_shape = shape("raw query must not leak", Some("bd-20bdb"), 4_000);
    let qos = SpecPackQosSnapshot {
        foreground_request_id_active: true,
        ..SpecPackQosSnapshot::idle()
    };
    let event = SpecPackTelemetryEvent::admission(
        "2026-05-20T09:30:00Z",
        &query_shape,
        qos,
        SpecPackAdmissionVerdict::DeniedForegroundRequestIdActive,
    );
    let value = serialized_event(&event)?;
    let encoded = serde_json::to_string(&value).map_err(|error| error.to_string())?;

    assert_eq!(value["schema"], SPEC_PACK_SCHEMA_V1);
    assert_eq!(value["sideEffectFree"], true);
    assert_eq!(value["phase"], "admission");
    assert_eq!(
        value["admissionVerdict"],
        "denied_foreground_request_id_active"
    );
    assert_eq!(value["degradedCodes"][0], SPEC_PACK_QOS_BACK_PRESSURE_CODE);
    assert!(
        value["workspaceIdHash"]
            .as_str()
            .is_some_and(|hash| hash.starts_with("blake3:"))
    );
    assert!(
        value["queryShapeHash"]
            .as_str()
            .is_some_and(|hash| hash.starts_with("blake3:"))
    );
    assert!(
        value["qosLaneSnapshotHash"]
            .as_str()
            .is_some_and(|hash| hash.starts_with("blake3:"))
    );
    assert!(!encoded.contains("raw query must not leak"));
    assert!(!encoded.contains("workspace-main"));
    assert!(!encoded.contains("CopperPrairie"));
    Ok(())
}

#[test]
fn run_store_and_abort_telemetry_use_pinned_phase_enums() -> TestResult {
    let query_shape = shape("prepare next pack", Some("bd-20bdb"), 8_000);
    let qos = SpecPackQosSnapshot::idle();
    let prepare = SpecPackTelemetryEvent::for_phase(
        "2026-05-20T09:30:59Z",
        SpecPackPhase::Prepare,
        &query_shape,
        qos,
        SpecPackAdmissionVerdict::Admitted,
    )
    .with_surface_pack_id("pack_speculative_0000");
    let prepare_json = serialized_event(&prepare)?;
    assert_eq!(prepare_json["phase"], "prepare");

    let run = SpecPackTelemetryEvent::for_phase(
        "2026-05-20T09:31:00Z",
        SpecPackPhase::Run,
        &query_shape,
        qos,
        SpecPackAdmissionVerdict::Admitted,
    )
    .with_surface_pack_id("pack_speculative_0001")
    .with_ttl_seconds(9_999)
    .with_tokens_used(123)
    .with_elapsed_ms(17);
    let run_json = serialized_event(&run)?;
    assert_eq!(run_json["phase"], "run");
    assert_eq!(run_json["surfacePackId"], "pack_speculative_0001");
    assert_eq!(run_json["ttlSeconds"], 3_600);
    assert_eq!(run_json["tokensUsed"], 123);
    assert_eq!(run_json["elapsedMs"], 17);
    assert_eq!(run_json["degradedCodes"].as_array().map(Vec::len), Some(0));

    let store = SpecPackTelemetryEvent::for_phase(
        "2026-05-20T09:31:00.500Z",
        SpecPackPhase::Store,
        &query_shape,
        qos,
        SpecPackAdmissionVerdict::Admitted,
    )
    .with_surface_pack_id("pack_speculative_0001");
    let store_json = serialized_event(&store)?;
    assert_eq!(store_json["phase"], "store");

    let abort = SpecPackTelemetryEvent::for_phase(
        "2026-05-20T09:31:01Z",
        SpecPackPhase::Abort,
        &query_shape,
        SpecPackQosSnapshot {
            active_speculations_for_workspace: 4,
            ..SpecPackQosSnapshot::idle()
        },
        SpecPackAdmissionVerdict::DeniedPerWorkspaceConcurrencyCap,
    )
    .with_abort_reason(SpecPackAbortReason::ForegroundRequestArrived);
    let abort_json = serialized_event(&abort)?;
    assert_eq!(abort_json["phase"], "abort");
    assert_eq!(abort_json["abortReason"], "foreground_request_arrived");
    assert_eq!(
        abort_json["degradedCodes"][0],
        SPEC_PACK_CACHE_ADMISSION_DENIED_CODE
    );
    for (reason, expected) in [
        (SpecPackAbortReason::Shutdown, "shutdown"),
        (SpecPackAbortReason::MemoryPressure, "memory_pressure"),
        (
            SpecPackAbortReason::TtlExpiredBeforeStore,
            "ttl_expired_before_store",
        ),
        (
            SpecPackAbortReason::SelectionNoLongerTopK,
            "selection_no_longer_top_k",
        ),
    ] {
        let value = serialized_event(
            &SpecPackTelemetryEvent::for_phase(
                "2026-05-20T09:31:02Z",
                SpecPackPhase::Abort,
                &query_shape,
                qos,
                SpecPackAdmissionVerdict::Admitted,
            )
            .with_abort_reason(reason),
        )?;
        assert_eq!(value["abortReason"], expected);
    }
    Ok(())
}
