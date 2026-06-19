//! bd-7lvbg.4 — property suite for the output-token governor engine
//! (`src/output/governor.rs`, ADR 0063, surface wiring bd-7lvbg.3).
//!
//! The engine's inline proptest pins only valid-JSON survival on a
//! uniform corpus. This file pins the contract the wired surfaces
//! (search, memory list, insights, curate, schema list) rely on, over
//! random heterogeneous corpora and random
//! ceilings:
//!
//! 1. **Validity + ceiling honesty** — governed output always parses;
//!    unless the response is `output_budget_unsatisfiable`-degraded,
//!    the stamped `meta.tokensEstimated` is ≤ the ceiling and the
//!    serialized bytes respect the anti-pathological byte backstop.
//! 2. **Prefix stability** — raising the ceiling never reorders or
//!    replaces what a smaller ceiling already emitted: the smaller
//!    page's elements are a prefix (flat arrays) / per-section prefix
//!    (round-robin shapes) of the larger page's.
//! 3. **Cursor-resume completeness** — draining pages to exhaustion
//!    partitions one generation's result set exactly: no duplicates,
//!    no gaps, flat order preserved, and the page sequence is
//!    byte-deterministic (drain twice → identical pages including
//!    cursors). `output_budget_unsatisfiable` may only ever appear on
//!    a fresh first page, never mid-sequence.
//! 4. **Rejected resume is an empty page** — a tampered cursor yields
//!    `cursor_invalid`, a generation advance yields `cursor_stale`,
//!    both with an emptied truncation point and no continuation
//!    cursor: a page sequence can never duplicate elements.
//! 5. **Cursor codec round-trip** — arbitrary payloads survive
//!    encode/decode under the right key and params hash; MAC, params,
//!    and generation violations reject exactly as ADR 0063 §3
//!    classifies them.

#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::output::governor::{
    CURSOR_INVALID_CODE, CURSOR_SCHEMA_V1, CURSOR_STALE_CODE, CursorPayload, CursorRejection,
    GovernorContext, OUTPUT_BUDGET_UNSATISFIABLE_CODE, OUTPUT_BYTE_BACKSTOP_MULTIPLIER,
    OUTPUT_TRUNCATED_BUDGET_CODE, TruncationPoint, decode_cursor, derive_workspace_mac_key,
    encode_cursor, govern_response_json_with_resume, hash_invocation_params,
};
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestCaseError};
use serde_json::{Value as JsonValue, json};

const FLAT_REGISTRY: &[TruncationPoint] = &[TruncationPoint {
    schema_id: "ee.proptest.list.v1",
    command: "proptest list",
    array_path: &["items"],
    per_section_items: false,
    position_key_field: "id",
}];

const SECTION_REGISTRY: &[TruncationPoint] = &[TruncationPoint {
    schema_id: "ee.proptest.sections.v1",
    command: "proptest sections",
    array_path: &["sections"],
    per_section_items: true,
    position_key_field: "id",
}];

const MAC_SCOPE: &str = "proptest-workspace";
const MAX_DRAIN_PAGES: usize = 512;

fn params_hash() -> String {
    hash_invocation_params(["proptest", "governor"])
}

fn flat_envelope(body_lens: &[usize]) -> (String, Vec<String>) {
    let ids: Vec<String> = (0..body_lens.len())
        .map(|index| format!("item_{index:04}"))
        .collect();
    let items: Vec<JsonValue> = ids
        .iter()
        .zip(body_lens)
        .map(|(id, len)| json!({ "id": id, "content": "x".repeat(*len) }))
        .collect();
    let envelope = json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "command": "proptest list",
            "schema": "ee.proptest.list.v1",
            "items": items,
        },
        "degraded": [],
    });
    (envelope.to_string(), ids)
}

fn sections_envelope(section_lens: &[Vec<usize>]) -> (String, Vec<String>) {
    let mut all_ids = Vec::new();
    let sections: Vec<JsonValue> = section_lens
        .iter()
        .enumerate()
        .map(|(section_index, lens)| {
            let items: Vec<JsonValue> = lens
                .iter()
                .enumerate()
                .map(|(item_index, len)| {
                    let id = format!("s{section_index:02}_item_{item_index:03}");
                    all_ids.push(id.clone());
                    json!({ "id": id, "content": "y".repeat(*len) })
                })
                .collect();
            json!({ "name": format!("section_{section_index:02}"), "items": items })
        })
        .collect();
    let envelope = json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "command": "proptest sections",
            "schema": "ee.proptest.sections.v1",
            "sections": sections,
        },
        "degraded": [],
    });
    (envelope.to_string(), all_ids)
}

fn parse(json: &str) -> Result<JsonValue, TestCaseError> {
    serde_json::from_str(json)
        .map_err(|error| TestCaseError::fail(format!("governed output is not JSON: {error}")))
}

fn degraded_entries(value: &JsonValue) -> Vec<JsonValue> {
    value
        .get("degraded")
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default()
}

fn has_degraded_code(value: &JsonValue, code: &str) -> bool {
    degraded_entries(value)
        .iter()
        .any(|entry| entry.get("code").and_then(JsonValue::as_str) == Some(code))
}

fn continuation_cursor(value: &JsonValue) -> Option<String> {
    degraded_entries(value).iter().find_map(|entry| {
        if entry.get("code").and_then(JsonValue::as_str) == Some(OUTPUT_TRUNCATED_BUDGET_CODE) {
            entry
                .pointer("/details/continuationCursor")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        } else {
            None
        }
    })
}

fn flat_ids(value: &JsonValue) -> Vec<String> {
    value
        .pointer("/data/items")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(JsonValue::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn section_item_ids(value: &JsonValue) -> Vec<String> {
    value
        .pointer("/data/sections")
        .and_then(JsonValue::as_array)
        .map(|sections| {
            sections
                .iter()
                .flat_map(|section| {
                    section
                        .get("items")
                        .and_then(JsonValue::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.get("id").and_then(JsonValue::as_str))
                                .map(str::to_owned)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn per_section_id_lists(value: &JsonValue) -> Vec<Vec<String>> {
    value
        .pointer("/data/sections")
        .and_then(JsonValue::as_array)
        .map(|sections| {
            sections
                .iter()
                .map(|section| {
                    section
                        .get("items")
                        .and_then(JsonValue::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.get("id").and_then(JsonValue::as_str))
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tokens_estimated(value: &JsonValue) -> Option<u64> {
    value
        .pointer("/meta/tokensEstimated")
        .and_then(JsonValue::as_u64)
}

fn govern_once(
    envelope: &str,
    ceiling: u64,
    registry: &[TruncationPoint],
    generation: u64,
    resume: Option<&str>,
) -> Result<String, TestCaseError> {
    let generation_fn = move || generation;
    let ctx = GovernorContext {
        ceiling_tokens: ceiling,
        params_hash: params_hash(),
        mac_key: derive_workspace_mac_key(MAC_SCOPE),
        db_generation: &generation_fn,
    };
    govern_response_json_with_resume(envelope, &ctx, registry, resume)
        .map_err(|error| TestCaseError::fail(format!("govern failed: {error:?}")))
}

/// Outcome of draining one envelope to exhaustion through cursor resume.
struct Drained {
    /// Per-page kept element ids, in emission order.
    pages: Vec<Vec<String>>,
    /// Raw governed page payloads (byte-determinism evidence).
    raw_pages: Vec<String>,
}

enum DrainOutcome {
    Drained(Drained),
    /// `output_budget_unsatisfiable` on the FRESH first page (legal).
    UnsatisfiableFirstPage,
}

fn drain_to_exhaustion(
    envelope: &str,
    ceiling: u64,
    registry: &[TruncationPoint],
    ids_of: fn(&JsonValue) -> Vec<String>,
    generation: u64,
) -> Result<DrainOutcome, TestCaseError> {
    let mut cursor: Option<String> = None;
    let mut pages = Vec::new();
    let mut raw_pages = Vec::new();
    for _ in 0..MAX_DRAIN_PAGES {
        let governed = govern_once(envelope, ceiling, registry, generation, cursor.as_deref())?;
        let value = parse(&governed)?;
        if has_degraded_code(&value, OUTPUT_BUDGET_UNSATISFIABLE_CODE) {
            if cursor.is_some() {
                return Err(TestCaseError::fail(
                    "output_budget_unsatisfiable mid-sequence: the envelope shell fit on an \
                     earlier page, so a remainder page must never fail closed",
                ));
            }
            return Ok(DrainOutcome::UnsatisfiableFirstPage);
        }
        if let Some(estimate) = tokens_estimated(&value) {
            if estimate > ceiling {
                return Err(TestCaseError::fail(format!(
                    "page estimate {estimate} exceeds ceiling {ceiling}"
                )));
            }
        } else {
            return Err(TestCaseError::fail(
                "governed page is missing meta.tokensEstimated",
            ));
        }
        if governed.len() as u64 > ceiling.saturating_mul(OUTPUT_BYTE_BACKSTOP_MULTIPLIER) {
            return Err(TestCaseError::fail(format!(
                "page byte length {} exceeds the byte backstop for ceiling {ceiling}",
                governed.len()
            )));
        }
        pages.push(ids_of(&value));
        raw_pages.push(governed);
        match continuation_cursor(&value) {
            Some(next) => cursor = Some(next),
            None => return Ok(DrainOutcome::Drained(Drained { pages, raw_pages })),
        }
    }
    Err(TestCaseError::fail(format!(
        "drain did not terminate within {MAX_DRAIN_PAGES} pages"
    )))
}

fn body_lens() -> impl Strategy<Value = Vec<usize>> {
    proptest::collection::vec(1usize..150, 0..60)
}

fn section_lens() -> impl Strategy<Value = Vec<Vec<usize>>> {
    proptest::collection::vec(proptest::collection::vec(1usize..80, 0..12), 1..6)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Property 1: any corpus under any ceiling yields parseable output
    /// whose stamped estimate honors the ceiling, or an honest
    /// fail-closed `output_budget_unsatisfiable` shell with no items
    /// and no continuation cursor.
    #[test]
    fn governed_output_is_valid_and_ceiling_honest(
        lens in body_lens(),
        ceiling in 8u64..2_500,
    ) {
        let (envelope, _) = flat_envelope(&lens);
        let governed = govern_once(&envelope, ceiling, FLAT_REGISTRY, 1, None)?;
        let value = parse(&governed)?;
        prop_assert_eq!(
            value.get("schema").and_then(JsonValue::as_str),
            Some("ee.response.v2"),
            "envelope schema must survive governing"
        );
        if has_degraded_code(&value, OUTPUT_BUDGET_UNSATISFIABLE_CODE) {
            prop_assert!(
                flat_ids(&value).is_empty(),
                "fail-closed shell must not carry truncation-point elements"
            );
            prop_assert!(
                continuation_cursor(&value).is_none(),
                "fail-closed shell must not offer a continuation cursor"
            );
        } else {
            let estimate = tokens_estimated(&value)
                .ok_or_else(|| TestCaseError::fail("missing meta.tokensEstimated"))?;
            prop_assert!(
                estimate <= ceiling,
                "stamped estimate {} exceeds ceiling {}", estimate, ceiling
            );
            prop_assert!(
                governed.len() as u64 <= ceiling.saturating_mul(OUTPUT_BYTE_BACKSTOP_MULTIPLIER),
                "serialized bytes {} exceed the byte backstop for ceiling {}",
                governed.len(),
                ceiling
            );
        }
    }

    /// Property 2 (flat): a smaller ceiling's kept elements are a
    /// prefix of a larger ceiling's kept elements on the same corpus.
    #[test]
    fn smaller_ceiling_output_is_a_prefix_of_larger(
        lens in body_lens(),
        base in 24u64..1_200,
        delta in 1u64..1_200,
    ) {
        let (envelope, _) = flat_envelope(&lens);
        let small = parse(&govern_once(&envelope, base, FLAT_REGISTRY, 1, None)?)?;
        let large = parse(
            &govern_once(&envelope, base.saturating_add(delta), FLAT_REGISTRY, 1, None)?,
        )?;
        let small_ids = flat_ids(&small);
        let large_ids = flat_ids(&large);
        prop_assert!(
            small_ids.len() <= large_ids.len(),
            "a larger ceiling kept fewer elements ({} vs {})",
            large_ids.len(),
            small_ids.len()
        );
        prop_assert_eq!(
            &small_ids[..],
            &large_ids[..small_ids.len()],
            "smaller-ceiling output is not a prefix of larger-ceiling output"
        );
    }

    /// Property 2 (per-section): under round-robin drops every
    /// section's kept items under a smaller ceiling are a prefix of
    /// that section's kept items under a larger ceiling.
    #[test]
    fn per_section_keeps_nest_across_ceilings(
        lens in section_lens(),
        base in 24u64..1_000,
        delta in 1u64..1_000,
    ) {
        let (envelope, _) = sections_envelope(&lens);
        let small = parse(&govern_once(&envelope, base, SECTION_REGISTRY, 1, None)?)?;
        let large = parse(
            &govern_once(&envelope, base.saturating_add(delta), SECTION_REGISTRY, 1, None)?,
        )?;
        let small_sections = per_section_id_lists(&small);
        let large_sections = per_section_id_lists(&large);
        if small_sections.is_empty() {
            // Fail-closed shell under the small ceiling: trivially nested.
            return Ok(());
        }
        prop_assert_eq!(
            small_sections.len(),
            large_sections.len(),
            "governing must never drop whole sections"
        );
        for (index, (small_items, large_items)) in
            small_sections.iter().zip(&large_sections).enumerate()
        {
            prop_assert!(
                small_items.len() <= large_items.len(),
                "section {} kept more under the smaller ceiling", index
            );
            prop_assert_eq!(
                &small_items[..],
                &large_items[..small_items.len()],
                "section {} keeps are not nested across ceilings", index
            );
        }
    }

    /// Property 4a: tampering with any part of a continuation cursor
    /// yields a `cursor_invalid` empty page — never a restarted
    /// sequence, so resumed pagination can never duplicate elements.
    #[test]
    fn tampered_cursor_resumes_as_an_empty_invalid_page(
        lens in proptest::collection::vec(8usize..150, 4..40),
        ceiling in 48u64..400,
    ) {
        let (envelope, _) = flat_envelope(&lens);
        let first = parse(&govern_once(&envelope, ceiling, FLAT_REGISTRY, 1, None)?)?;
        let Some(cursor) = continuation_cursor(&first) else {
            // Everything fit (or the shell failed closed): no cursor to tamper with.
            return Ok(());
        };
        let mut tampered: String = cursor.clone();
        let head = tampered.remove(0);
        tampered.insert(0, if head == 'A' { 'B' } else { 'A' });
        let resumed = parse(&govern_once(
            &envelope,
            ceiling,
            FLAT_REGISTRY,
            1,
            Some(&tampered),
        )?)?;
        prop_assert!(
            has_degraded_code(&resumed, CURSOR_INVALID_CODE),
            "tampered cursor must be rejected as cursor_invalid"
        );
        prop_assert!(
            flat_ids(&resumed).is_empty(),
            "a rejected cursor must yield an empty page, not a restart"
        );
        prop_assert!(
            continuation_cursor(&resumed).is_none(),
            "a rejected-cursor page must not offer a continuation cursor"
        );
    }

    /// Property 4b: a generation advance between pages yields a
    /// `cursor_stale` empty page — resumed sequences never silently
    /// mix two generations' result sets.
    #[test]
    fn generation_advance_resumes_as_an_empty_stale_page(
        lens in proptest::collection::vec(8usize..150, 4..40),
        ceiling in 48u64..400,
    ) {
        let (envelope, _) = flat_envelope(&lens);
        let first = parse(&govern_once(&envelope, ceiling, FLAT_REGISTRY, 1, None)?)?;
        let Some(cursor) = continuation_cursor(&first) else {
            return Ok(());
        };
        let resumed = parse(&govern_once(
            &envelope,
            ceiling,
            FLAT_REGISTRY,
            2,
            Some(&cursor),
        )?)?;
        prop_assert!(
            has_degraded_code(&resumed, CURSOR_STALE_CODE),
            "a generation advance must be rejected as cursor_stale"
        );
        prop_assert!(
            flat_ids(&resumed).is_empty(),
            "a stale cursor must yield an empty page, not a restart"
        );
        prop_assert!(
            continuation_cursor(&resumed).is_none(),
            "a stale-cursor page must not offer a continuation cursor"
        );
    }
}

proptest! {
    // Drains run the estimator across every page of every case; a
    // lower case count keeps the suite inside unit-test budgets while
    // still exploring corpus × ceiling space.
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Property 3 (flat): cursor pages partition the full result set
    /// exactly — concatenated page ids equal the original id sequence
    /// (no duplicates, no gaps, order preserved) — and the page
    /// sequence is byte-deterministic.
    #[test]
    fn flat_drain_partitions_exactly_and_deterministically(
        lens in proptest::collection::vec(1usize..150, 1..50),
        ceiling in 32u64..1_500,
    ) {
        let (envelope, full_ids) = flat_envelope(&lens);
        let outcome = drain_to_exhaustion(&envelope, ceiling, FLAT_REGISTRY, flat_ids, 7)?;
        let DrainOutcome::Drained(first) = outcome else {
            return Ok(());
        };
        let drained: Vec<String> = first.pages.iter().flatten().cloned().collect();
        prop_assert_eq!(
            &drained,
            &full_ids,
            "drained pages must concatenate to exactly the full id sequence"
        );
        let DrainOutcome::Drained(second) =
            drain_to_exhaustion(&envelope, ceiling, FLAT_REGISTRY, flat_ids, 7)?
        else {
            return Err(TestCaseError::fail(
                "second drain failed closed where the first drained",
            ));
        };
        prop_assert_eq!(
            &first.raw_pages,
            &second.raw_pages,
            "page sequences must be byte-identical across identical drains"
        );
    }

    /// Property 3 (per-section): resumed round-robin pages partition
    /// the full element set exactly — no element is duplicated or
    /// lost across pages.
    #[test]
    fn per_section_drain_partitions_exactly(
        lens in section_lens(),
        ceiling in 32u64..1_200,
    ) {
        let (envelope, full_ids) = sections_envelope(&lens);
        let outcome =
            drain_to_exhaustion(&envelope, ceiling, SECTION_REGISTRY, section_item_ids, 11)?;
        let DrainOutcome::Drained(drained) = outcome else {
            return Ok(());
        };
        let mut seen = std::collections::BTreeSet::new();
        for (page_index, page) in drained.pages.iter().enumerate() {
            for id in page {
                prop_assert!(
                    seen.insert(id.clone()),
                    "element {} appeared twice (second time on page {})", id, page_index
                );
            }
        }
        let expected: std::collections::BTreeSet<String> = full_ids.into_iter().collect();
        prop_assert_eq!(
            seen,
            expected,
            "drained pages must cover exactly the full element set"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Property 5: the cursor codec round-trips arbitrary payloads and
    /// classifies violations exactly (ADR 0063 §3): wrong MAC key and
    /// params mismatch are `Invalid`, future generations are
    /// `Invalid`, older generations are `Stale`.
    #[test]
    fn cursor_codec_round_trips_and_classifies_rejections(
        target_schema in "[a-z.]{1,32}",
        db_generation in 0u64..1_000_000,
        position_key in proptest::collection::vec(any::<char>(), 0..24),
        dropped_count in 0u64..1_000_000,
    ) {
        let mac_key = derive_workspace_mac_key(MAC_SCOPE);
        let params = params_hash();
        let payload = CursorPayload {
            schema: CURSOR_SCHEMA_V1.to_string(),
            target_schema,
            db_generation,
            position_key: position_key.into_iter().collect(),
            dropped_count,
            params_hash: params.clone(),
        };
        let token = encode_cursor(&payload, &mac_key)
            .map_err(|error| TestCaseError::fail(format!("encode failed: {error:?}")))?;

        prop_assert_eq!(
            decode_cursor(&token, &mac_key, &params, db_generation),
            Ok(payload.clone()),
            "same key, params, and generation must round-trip"
        );
        prop_assert_eq!(
            decode_cursor(&token, &derive_workspace_mac_key("other-scope"), &params, db_generation),
            Err(CursorRejection::Invalid),
            "a different workspace key must reject as Invalid"
        );
        prop_assert_eq!(
            decode_cursor(
                &token,
                &mac_key,
                &hash_invocation_params(["different", "params"]),
                db_generation,
            ),
            Err(CursorRejection::Invalid),
            "a params mismatch must reject as Invalid"
        );
        if db_generation > 0 {
            prop_assert_eq!(
                decode_cursor(&token, &mac_key, &params, db_generation - 1),
                Err(CursorRejection::Invalid),
                "a cursor from the future must reject as Invalid"
            );
        }
        prop_assert_eq!(
            decode_cursor(&token, &mac_key, &params, db_generation + 1),
            Err(CursorRejection::Stale {
                cursor_generation: db_generation,
                current_generation: db_generation + 1,
            }),
            "an advanced generation must reject as Stale"
        );
    }
}
