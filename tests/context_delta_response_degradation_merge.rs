//! bd-270ep: lock the contract that the `ee context --since` happy-path
//! merges response-side degradations (the `deprecated_alias` every
//! `ee context` invocation carries, plus pack-assembly degradations
//! attached upstream by `run_context_pack`) onto the agent-visible
//! `ContextDeltaEnvelope.degraded[]` before serialization.
//!
//! The CLI helper `maybe_write_context_delta` is not part of the public
//! library surface, but the `append_response_degradation` API it relies
//! on is. These tests exercise that API directly and assert the
//! serialized envelope shape an agent would receive on stdout. The
//! companion CLI commit at the same beadprint that this test runs
//! against threads `response.data.degraded` through
//! `delta.append_response_degradation(...)` immediately before
//! `serde_json::to_string(&delta)`; if that loop is removed or weakened,
//! these tests stay green only by accident — keep them tight.

use serde_json::Value;

use ee::core::context_delta::{
    CONTEXT_DELTA_SCHEMA_V1, ContextDeltaItemSnapshot, ContextDeltaOptions,
    ContextDeltaPackSnapshot, compute_context_delta,
};

type TestResult = Result<(), String>;

fn happy_path_envelope_with_one_modified_item()
-> Result<ee::core::context_delta::ContextDeltaEnvelope, String> {
    // One modified item is enough to land on the happy path (emits_delta()
    // == true) without crossing the oversized fallback threshold.
    let prior = ContextDeltaPackSnapshot::new(
        "h1",
        1,
        1024,
        320,
        vec![
            ContextDeltaItemSnapshot::new("mem_a")
                .with_field("contentHash", Value::String("old-a".to_string()))
                .with_field("estimatedTokens", serde_json::json!(10)),
        ],
    );
    let new = ContextDeltaPackSnapshot::new(
        "h2",
        2,
        1024,
        320,
        vec![
            ContextDeltaItemSnapshot::new("mem_a")
                .with_field("contentHash", Value::String("new-a".to_string()))
                .with_field("estimatedTokens", serde_json::json!(12)),
        ],
    );
    compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
        .map_err(|error| format!("compute_context_delta: {error}"))
}

#[test]
fn happy_path_envelope_starts_empty_so_the_merge_is_the_only_source_of_response_degradations()
-> TestResult {
    // Sanity-pin: without the merge the kernel envelope's degraded[] is
    // empty on the happy path. If this ever flips to non-empty, the
    // positive assertions below would pass for the wrong reason.
    let envelope = happy_path_envelope_with_one_modified_item()?;
    assert!(envelope.emits_delta(), "happy-path delta expected");
    assert!(
        envelope.degraded.is_empty(),
        "kernel envelope must start with degraded[] empty on the happy path; saw {:?}",
        envelope.degraded
    );
    Ok(())
}

#[test]
fn append_response_degradation_lands_in_envelope_degraded_array() -> TestResult {
    let mut envelope = happy_path_envelope_with_one_modified_item()?;

    envelope.append_response_degradation(
        "deprecated_alias",
        "info",
        "`ee context` is a compatibility alias for the promoted triad command.",
        Some("Use `ee pack \"<task>\"`.".to_string()),
    );
    envelope.append_response_degradation(
        "pack_assembly_slow",
        "low",
        "Pack assembly exceeded the standard SLO budget.",
        Some("Retry with --resource-profile lean or expand the budget.".to_string()),
    );

    assert_eq!(
        envelope.degraded.len(),
        2,
        "both response degradations must be projected; saw {} entries: {:?}",
        envelope.degraded.len(),
        envelope.degraded
    );

    let codes: Vec<&str> = envelope
        .degraded
        .iter()
        .map(|entry| entry.code.as_str())
        .collect();
    assert!(
        codes.contains(&"deprecated_alias"),
        "deprecated_alias must appear in delta.degraded; got {codes:?}"
    );
    assert!(
        codes.contains(&"pack_assembly_slow"),
        "pack_assembly_slow must appear in delta.degraded; got {codes:?}"
    );

    let deprecated = envelope
        .degraded
        .iter()
        .find(|entry| entry.code == "deprecated_alias")
        .ok_or_else(|| "deprecated_alias entry vanished".to_string())?;
    assert_eq!(
        deprecated.severity, "info",
        "severity round-trips as string"
    );
    assert_eq!(
        deprecated.repair.as_deref(),
        Some("Use `ee pack \"<task>\"`."),
        "repair text round-trips",
    );

    let slow = envelope
        .degraded
        .iter()
        .find(|entry| entry.code == "pack_assembly_slow")
        .ok_or_else(|| "pack_assembly_slow entry vanished".to_string())?;
    assert_eq!(slow.severity, "low");
    Ok(())
}

#[test]
fn merged_degradations_serialize_as_top_level_degraded_array_per_schema() -> TestResult {
    let mut envelope = happy_path_envelope_with_one_modified_item()?;
    envelope.append_response_degradation(
        "deprecated_alias",
        "info",
        "`ee context` is a compatibility alias for the promoted triad command.",
        Some("Use `ee pack \"<task>\"`.".to_string()),
    );
    envelope.append_response_degradation(
        "pack_assembly_slow",
        "low",
        "Pack assembly exceeded the standard SLO budget.",
        None,
    );

    let rendered =
        serde_json::to_value(&envelope).map_err(|error| format!("serialize envelope: {error}"))?;
    assert_eq!(
        rendered["schema"].as_str(),
        Some(CONTEXT_DELTA_SCHEMA_V1),
        "schema field unchanged by merge",
    );
    let degraded = rendered["degraded"]
        .as_array()
        .ok_or_else(|| "top-level degraded must serialize as an array".to_string())?;
    assert_eq!(
        degraded.len(),
        2,
        "serialized envelope must carry both merged entries; got {degraded:?}"
    );
    let codes: Vec<&str> = degraded
        .iter()
        .filter_map(|entry| entry["code"].as_str())
        .collect();
    assert!(
        codes.contains(&"deprecated_alias") && codes.contains(&"pack_assembly_slow"),
        "serialized degraded[] must contain both codes; got {codes:?}",
    );

    // Repair is `skip_serializing_if = Option::is_none` on
    // ContextDeltaDegradation, so the no-repair entry must omit the key
    // entirely rather than serialize a null — guards against agent
    // parsers that distinguish missing-from-null.
    let slow = degraded
        .iter()
        .find(|entry| entry["code"].as_str() == Some("pack_assembly_slow"))
        .ok_or_else(|| "pack_assembly_slow missing in serialized output".to_string())?;
    assert!(
        !slow.as_object().expect("object").contains_key("repair"),
        "no-repair entry must omit the `repair` key, not serialize a null; saw {slow}"
    );
    Ok(())
}

#[test]
fn append_response_degradation_preserves_existing_kernel_degraded_entries() -> TestResult {
    // If a future change keeps the merge but reorders it relative to the
    // kernel's own `degraded[]` population (e.g. on a fallback path that
    // also calls into the happy-path merge), the kernel-side entries
    // must not get clobbered.
    let mut envelope = happy_path_envelope_with_one_modified_item()?;
    let kernel_entry = ee::core::context_delta::ContextDeltaDegradation {
        code: "kernel_synthetic".to_string(),
        severity: "info".to_string(),
        message: "preserved across merge".to_string(),
        repair: None,
        details: None,
    };
    envelope.degraded.push(kernel_entry);
    envelope.append_response_degradation(
        "deprecated_alias",
        "info",
        "`ee context` is a compatibility alias for the promoted triad command.",
        Some("Use `ee pack \"<task>\"`.".to_string()),
    );
    assert_eq!(envelope.degraded.len(), 2);
    assert_eq!(envelope.degraded[0].code, "kernel_synthetic");
    assert_eq!(envelope.degraded[1].code, "deprecated_alias");
    Ok(())
}
