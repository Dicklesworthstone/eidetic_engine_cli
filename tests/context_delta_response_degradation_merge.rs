//! bd-270ep: lock the contract that the `ee pack --since` happy-path
//! merges response-side degradations (pack-assembly degradations
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
        "semantic_disabled",
        "info",
        "Semantic retrieval is disabled; ranking used the lexical tier only.",
        Some("Run `ee doctor --json` to inspect the embedding tier.".to_string()),
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
        codes.contains(&"semantic_disabled"),
        "semantic_disabled must appear in delta.degraded; got {codes:?}"
    );
    assert!(
        codes.contains(&"pack_assembly_slow"),
        "pack_assembly_slow must appear in delta.degraded; got {codes:?}"
    );

    let semantic = envelope
        .degraded
        .iter()
        .find(|entry| entry.code == "semantic_disabled")
        .ok_or_else(|| "semantic_disabled entry vanished".to_string())?;
    assert_eq!(semantic.severity, "info", "severity round-trips as string");
    assert_eq!(
        semantic.repair.as_deref(),
        Some("Run `ee doctor --json` to inspect the embedding tier."),
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
        "semantic_disabled",
        "info",
        "Semantic retrieval is disabled; ranking used the lexical tier only.",
        Some("Run `ee doctor --json` to inspect the embedding tier.".to_string()),
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
        codes.contains(&"semantic_disabled") && codes.contains(&"pack_assembly_slow"),
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
        "semantic_disabled",
        "info",
        "Semantic retrieval is disabled; ranking used the lexical tier only.",
        Some("Run `ee doctor --json` to inspect the embedding tier.".to_string()),
    );
    assert_eq!(envelope.degraded.len(), 2);
    assert_eq!(envelope.degraded[0].code, "kernel_synthetic");
    assert_eq!(envelope.degraded[1].code, "semantic_disabled");
    Ok(())
}

// ---------------------------------------------------------------------------
// bd-xm5qz: post-merge budget + tokenSavings re-measurement
// ---------------------------------------------------------------------------

const CONTEXT_DELTA_OVERSIZED_CODE: &str = "context_delta_larger_than_full";

fn merge_realistic_response_degradations(
    envelope: &mut ee::core::context_delta::ContextDeltaEnvelope,
) {
    // Three entries roughly mirror the worst common case: three
    // pack-pipeline signals (semantic disabled, BM25-only fallback, slow
    // pack assembly) that run_context_pack routinely attaches when search
    // or assembly degrades.
    envelope.append_response_degradation(
        "semantic_disabled",
        "info",
        "Semantic retrieval is disabled; ranking used the lexical tier only.",
        Some("Run `ee doctor --json` to inspect the embedding tier.".to_string()),
    );
    envelope.append_response_degradation(
        "search_lexical_only",
        "low",
        "Semantic search was unavailable; ranking used BM25 only.",
        Some("Run `ee doctor --json` to inspect the embedding tier.".to_string()),
    );
    envelope.append_response_degradation(
        "pack_assembly_slow",
        "low",
        "Pack assembly exceeded the standard SLO budget.",
        Some("Retry with --resource-profile lean or expand the budget.".to_string()),
    );
}

#[test]
fn finalize_after_merge_updates_token_savings_to_post_merge_bytes() -> TestResult {
    // No budget configured — we just want to prove tokenSavings.deltaBytes
    // matches the bytes the agent actually receives once the merged
    // degradations are accounted for.
    let mut envelope = happy_path_envelope_with_one_modified_item()?;
    let kernel_bytes = envelope.data.token_savings.delta_bytes;
    let kernel_saved = envelope.data.token_savings.saved_bytes;

    merge_realistic_response_degradations(&mut envelope);

    let final_bytes = envelope
        .finalize_with_budget(None)
        .map_err(|error| format!("finalize_with_budget failed: {error}"))?;

    let serialized =
        serde_json::to_vec(&envelope).map_err(|error| format!("serialize envelope: {error}"))?;
    let emitted_bytes = serialized.len() as u64;
    assert_eq!(
        final_bytes, emitted_bytes,
        "finalize_with_budget must return the actual emission size"
    );
    assert_eq!(
        envelope.data.token_savings.delta_bytes, emitted_bytes,
        "tokenSavings.deltaBytes must report the post-merge emission size, not the pre-merge kernel measurement",
    );
    assert!(
        envelope.data.token_savings.delta_bytes > kernel_bytes,
        "post-merge bytes ({}) must exceed the kernel's pre-merge measurement ({})",
        envelope.data.token_savings.delta_bytes,
        kernel_bytes,
    );
    assert_eq!(
        envelope.data.token_savings.saved_bytes,
        envelope.data.token_savings.full_bytes as i64 - emitted_bytes as i64,
        "savedBytes must reflect the post-merge emission",
    );
    assert!(
        envelope.data.token_savings.saved_bytes < kernel_saved,
        "post-merge savedBytes ({}) must be smaller than the pre-merge value ({}) because the emission grew",
        envelope.data.token_savings.saved_bytes,
        kernel_saved,
    );
    assert!(
        envelope.emits_delta(),
        "without a budget the envelope must still emit a delta",
    );
    Ok(())
}

#[test]
fn finalize_after_merge_respects_tight_max_delta_bytes_budget() -> TestResult {
    // Construct an envelope whose kernel-measured size fits under the
    // configured budget but whose post-merge size does NOT, then prove
    // finalize_with_budget flips to the oversize fallback path so the
    // CLI can fall through to the full pack instead of silently emitting
    // bytes above the agent's stated budget.
    let mut envelope = happy_path_envelope_with_one_modified_item()?;
    let kernel_bytes = envelope.data.token_savings.delta_bytes;

    merge_realistic_response_degradations(&mut envelope);

    let post_merge_bytes = serde_json::to_vec(&envelope)
        .map_err(|error| format!("serialize envelope: {error}"))?
        .len() as u64;
    assert!(
        post_merge_bytes > kernel_bytes,
        "test precondition: merge must enlarge the envelope ({kernel_bytes} -> {post_merge_bytes})",
    );

    // Budget sits between the kernel's pre-merge size and the post-merge
    // size — the kernel would have accepted it; the merge pushes it
    // over.
    let budget = (kernel_bytes + post_merge_bytes) / 2;
    assert!(budget > kernel_bytes && budget < post_merge_bytes);

    let final_bytes = envelope
        .finalize_with_budget(Some(budget))
        .map_err(|error| format!("finalize_with_budget failed: {error}"))?;

    assert!(
        !envelope.emits_delta(),
        "envelope must flip to fallback when post-merge size exceeds the budget",
    );
    assert_eq!(
        envelope.data.server_decision.fallback_reason,
        Some(ee::core::context_delta::ContextDeltaFallbackReason::DeltaLargerThanFull),
        "fallbackReason must be DeltaLargerThanFull",
    );
    let oversized_count = envelope
        .degraded
        .iter()
        .filter(|entry| entry.code == CONTEXT_DELTA_OVERSIZED_CODE)
        .count();
    assert_eq!(
        oversized_count, 1,
        "exactly one CONTEXT_DELTA_OVERSIZED_CODE entry must be present after finalize",
    );
    let serialized =
        serde_json::to_vec(&envelope).map_err(|error| format!("serialize envelope: {error}"))?;
    assert_eq!(
        final_bytes,
        serialized.len() as u64,
        "finalize_with_budget must return the size of the emission it produced",
    );
    assert_eq!(
        envelope.data.token_savings.delta_bytes,
        serialized.len() as u64,
        "tokenSavings.deltaBytes must report the bytes the agent actually receives, including the oversize-marker entry the finalize step pushed",
    );
    Ok(())
}

#[test]
fn finalize_with_budget_is_idempotent_when_called_twice() -> TestResult {
    // Guards against a future change where finalize_with_budget gets
    // called more than once (e.g. an additional merge stage) — the
    // CONTEXT_DELTA_OVERSIZED_CODE marker must not duplicate and the
    // serialized size must converge.
    let mut envelope = happy_path_envelope_with_one_modified_item()?;
    merge_realistic_response_degradations(&mut envelope);
    let post_merge_bytes = serde_json::to_vec(&envelope)
        .map_err(|error| format!("serialize envelope: {error}"))?
        .len() as u64;
    let kernel_bytes = envelope.data.token_savings.delta_bytes;
    let budget = (kernel_bytes + post_merge_bytes) / 2;

    let first = envelope
        .finalize_with_budget(Some(budget))
        .map_err(|error| format!("finalize_with_budget pass 1 failed: {error}"))?;
    let second = envelope
        .finalize_with_budget(Some(budget))
        .map_err(|error| format!("finalize_with_budget pass 2 failed: {error}"))?;
    assert_eq!(first, second, "finalize_with_budget must converge");
    let oversized_count = envelope
        .degraded
        .iter()
        .filter(|entry| entry.code == CONTEXT_DELTA_OVERSIZED_CODE)
        .count();
    assert_eq!(
        oversized_count, 1,
        "double-call must not duplicate the oversize marker",
    );
    Ok(())
}

// bd-2pgex: the CLI emission appends `\n` to the serialized envelope.
// `finalize_with_budget_and_transport_overhead` must account for that
// extra byte so the actual stdout size stays ≤ max_delta_bytes at the
// boundary — and `tokenSavings.deltaBytes` must report the same number
// the budget check enforced (no PRE-marker vs POST-marker disagreement
// for drift-1).
#[test]
fn finalize_with_transport_overhead_holds_emission_at_or_under_budget() -> TestResult {
    let mut envelope = happy_path_envelope_with_one_modified_item()?;
    merge_realistic_response_degradations(&mut envelope);

    let serialized_body_bytes = serde_json::to_vec(&envelope)
        .map_err(|error| format!("serialize envelope: {error}"))?
        .len() as u64;
    let overhead: u64 = 1; // matches the CLI's trailing `\n`
    let exact_budget = serialized_body_bytes + overhead;

    // Boundary: budget == body + overhead → the envelope still emits
    // (no fallback) and stdout size <= budget exactly.
    let final_bytes = envelope
        .finalize_with_budget_and_transport_overhead(Some(exact_budget), overhead)
        .map_err(|error| format!("finalize at boundary failed: {error}"))?;
    assert!(
        envelope.emits_delta(),
        "envelope must still emit at the exact budget boundary",
    );
    assert_eq!(
        final_bytes, exact_budget,
        "final bytes at boundary must equal body + transport overhead",
    );
    assert_eq!(
        envelope.data.token_savings.delta_bytes, exact_budget,
        "tokenSavings.deltaBytes must reflect the post-overhead emission size",
    );
    // (a) total bytes ≤ max_delta_bytes — including the trailing
    // newline the CLI will append after finalize returns.
    let post_finalize_body_bytes = serde_json::to_vec(&envelope)
        .map_err(|error| format!("re-serialize envelope: {error}"))?
        .len() as u64;
    assert!(
        post_finalize_body_bytes + overhead <= exact_budget,
        "post-finalize stdout size ({} body + {} overhead) must fit max_delta_bytes={}",
        post_finalize_body_bytes,
        overhead,
        exact_budget,
    );

    // Now squeeze the budget by one byte — finalize must flip to
    // fallback and the marker count must be exactly 1.
    let mut tight = happy_path_envelope_with_one_modified_item()?;
    merge_realistic_response_degradations(&mut tight);
    let serialized_tight_body = serde_json::to_vec(&tight)
        .map_err(|error| format!("serialize tight envelope: {error}"))?
        .len() as u64;
    let tight_budget = serialized_tight_body; // one byte tighter than body+overhead
    let tight_final = tight
        .finalize_with_budget_and_transport_overhead(Some(tight_budget), overhead)
        .map_err(|error| format!("finalize tight failed: {error}"))?;
    assert!(
        !tight.emits_delta(),
        "envelope must flip to fallback when body+overhead > budget by one byte",
    );
    let tight_oversized_count = tight
        .degraded
        .iter()
        .filter(|entry| entry.code == CONTEXT_DELTA_OVERSIZED_CODE)
        .count();
    // (b) marker count == 1 (single source of truth for the oversize
    // signal); tokenSavings.deltaBytes == the actual emission size,
    // not the PRE-marker measurement.
    assert_eq!(
        tight_oversized_count, 1,
        "tight-budget overflow must push exactly one oversize marker",
    );
    let tight_body_after_marker = serde_json::to_vec(&tight)
        .map_err(|error| format!("re-serialize tight envelope: {error}"))?
        .len() as u64;
    assert_eq!(
        tight.data.token_savings.delta_bytes,
        tight_body_after_marker + overhead,
        "tokenSavings.deltaBytes must equal the post-marker body + transport overhead",
    );
    assert_eq!(
        tight_final,
        tight_body_after_marker + overhead,
        "returned size must match tokenSavings.deltaBytes",
    );

    // Drift-1 sanity: the marker message must NOT interpolate a stale
    // PRE-marker byte count that would disagree with deltaBytes.
    let marker_message = tight
        .degraded
        .iter()
        .find(|entry| entry.code == CONTEXT_DELTA_OVERSIZED_CODE)
        .map(|entry| entry.message.clone())
        .ok_or_else(|| "oversize marker missing".to_string())?;
    // The new message references the budget but not a fixed byte count
    // that would drift away from tokenSavings.deltaBytes.
    assert!(
        marker_message.contains(&tight_budget.to_string()),
        "marker should reference the configured budget; got: {marker_message}"
    );
    assert!(
        marker_message.contains("data.tokenSavings.deltaBytes"),
        "marker should point at data.tokenSavings.deltaBytes as the single source of truth; got: {marker_message}"
    );
    Ok(())
}
