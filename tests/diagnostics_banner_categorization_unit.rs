//! E2 — DegradedCategory mapping coverage (bd-17c65.5.2).
//!
//! Asserts the deterministic mapping from a degraded `code` string to a
//! [`DegradedCategory`]:
//!
//!   1. Every code listed in `KNOWN_CODE_EXPECTATIONS` below maps to its
//!      documented category. A failure here means the implementation
//!      drifted from the bead's spec table — fix the impl, not this test.
//!   2. Unknown codes fall through to `AffectsThisResponse` so a newly
//!      added code that has not been categorized yet is surfaced
//!      conservatively (visible to the agent) rather than silently
//!      filtered. This is the safe default per the bead's design note.
//!   3. The three category variants serialize to stable snake_case
//!      strings that downstream consumers can switch on.
//!
//! This is the canonical regression guard against `degraded_context`-style
//! meta-codes sneaking back in (the meta-banner was deleted as part of
//! E2; if any code with that name shows up, the renderer would now
//! filter it as `AffectsThisResponse` default but the deletion gate in
//! `diagnostics_banner_aliasing_unit.rs` catches the actual emission
//! regression).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::pack::{DegradedCategory, category_for_code};

type TestResult = Result<(), String>;

/// The canonical category mapping table the bead spec'd in plain prose.
/// Encoded here as data so the test fails loudly if anyone changes a
/// row without updating the spec, OR changes the spec without updating
/// the impl.
///
/// Each row: (code, expected_category, rationale_for_the_audit_trail).
const KNOWN_CODE_EXPECTATIONS: &[(&str, DegradedCategory, &str)] = &[
    // BUILD-TIME FEATURE GAPS — feature not compiled into the binary.
    // Belongs in `ee capabilities`, NOT per-response.
    (
        "graph_snapshot_unimplemented",
        DegradedCategory::BuildTimeFeatureGap,
        "graph feature not compiled in — capability gap, not response",
    ),
    (
        "mcp_feature_disabled",
        DegradedCategory::BuildTimeFeatureGap,
        "mcp adapter requires --features mcp at build time",
    ),
    (
        "mcp_unavailable",
        DegradedCategory::BuildTimeFeatureGap,
        "mcp surface not present in this binary",
    ),
    (
        "diagram_backend_unavailable",
        DegradedCategory::BuildTimeFeatureGap,
        "mermaid/diagram render backend not linked",
    ),
    // WORKSPACE-STATE conditions — observable via `ee status`, not
    // specific to the current response.
    (
        "search_index_stale",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "DB has moved past index gen; surface via ee status, not per-pack",
    ),
    (
        "index_stale",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "shorter alias for search_index_stale",
    ),
    (
        "index_missing",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "index dir not present — workspace state, not response state",
    ),
    (
        "index_corrupt",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "index corruption — workspace concern",
    ),
    (
        "index_locked",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "index lock held — workspace concern",
    ),
    (
        "cass_unavailable",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "cass binary missing — affects import, not most commands",
    ),
    (
        "graph_snapshot_missing",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "graph snapshot not built yet — workspace concern",
    ),
    (
        "graph_snapshot_stale",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "graph snapshot lagging memory writes",
    ),
    (
        "graph_snapshot_topology_unavailable",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "topology metrics not yet computed for this snapshot",
    ),
    (
        "graph_snapshot_unusable",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "graph snapshot present but malformed — workspace recoverable",
    ),
    (
        "graph_unavailable",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "graph subsystem not initialized",
    ),
    (
        "agent_detection_unavailable",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "agent-detection probe failed — workspace-wide state",
    ),
    (
        "model_registry_empty",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "no embedder registered — workspace config concern",
    ),
    (
        "model_registry_no_available_entry",
        DegradedCategory::WorkspaceStateNotPerResponse,
        "registry populated but no available entry — workspace state",
    ),
    // AFFECTS THIS RESPONSE — the current response was affected.
    // Always emit per the bead spec.
    (
        "no_relevant_results",
        DegradedCategory::AffectsThisResponse,
        "B1 — query returned nothing above the floor; ALWAYS relevant",
    ),
    (
        "weak_query_recall",
        DegradedCategory::AffectsThisResponse,
        "B5 — top score below 2× floor; ALWAYS relevant when emitted",
    ),
    (
        "low_recall_after_floor",
        DegradedCategory::AffectsThisResponse,
        "drop rate exceeded; ALWAYS relevant when emitted",
    ),
    (
        "duplicates_collapsed",
        DegradedCategory::AffectsThisResponse,
        "search dedupe fired on this response",
    ),
    (
        "context_pack_persist_failed",
        DegradedCategory::AffectsThisResponse,
        "this pack's persistence write failed — response-affecting",
    ),
    (
        "context_profile_budget_capped",
        DegradedCategory::AffectsThisResponse,
        "active profile capped THIS request's budget",
    ),
];

#[test]
fn every_known_code_maps_to_documented_category() -> TestResult {
    for (code, expected, rationale) in KNOWN_CODE_EXPECTATIONS {
        let actual = category_for_code(code);
        if actual != *expected {
            return Err(format!(
                "code `{code}` mapped to {:?}, expected {:?} ({rationale})",
                actual, expected,
            ));
        }
    }
    Ok(())
}

#[test]
fn unknown_codes_default_to_affects_this_response() -> TestResult {
    // The conservative default for newly-added codes that haven't been
    // categorized yet: surface them so an agent sees the signal. The
    // alternative (silent filtering) is the failure mode the bead was
    // filed to eliminate; this test pins the safe default.
    let unknown_codes = [
        "freshly_invented_code_42",
        "future_feature_warning",
        "",  // empty string defaults too — not a special case
        "WEIRD_CASE_THAT_DOES_NOT_EXIST_YET",
        "snake_case_unknown",
        "camelCaseUnknown",
    ];
    for code in unknown_codes {
        let actual = category_for_code(code);
        if actual != DegradedCategory::AffectsThisResponse {
            return Err(format!(
                "unknown code `{code}` should default to AffectsThisResponse, got {:?}",
                actual,
            ));
        }
    }
    Ok(())
}

#[test]
fn category_as_str_emits_stable_snake_case_identifiers() -> TestResult {
    let cases = [
        (
            DegradedCategory::AffectsThisResponse,
            "affects_this_response",
        ),
        (
            DegradedCategory::WorkspaceStateNotPerResponse,
            "workspace_state_not_per_response",
        ),
        (DegradedCategory::BuildTimeFeatureGap, "build_time_feature_gap"),
    ];
    for (variant, expected) in cases {
        if variant.as_str() != expected {
            return Err(format!(
                "{:?}.as_str() = {:?}, expected {:?}",
                variant,
                variant.as_str(),
                expected,
            ));
        }
    }
    Ok(())
}

#[test]
fn only_affects_this_response_is_included_by_default() -> TestResult {
    if !DegradedCategory::AffectsThisResponse.included_by_default() {
        return Err("AffectsThisResponse must be included by default".to_string());
    }
    if DegradedCategory::WorkspaceStateNotPerResponse.included_by_default() {
        return Err("WorkspaceStateNotPerResponse must NOT be included by default".to_string());
    }
    if DegradedCategory::BuildTimeFeatureGap.included_by_default() {
        return Err("BuildTimeFeatureGap must NOT be included by default".to_string());
    }
    Ok(())
}

#[test]
fn known_code_table_has_no_duplicates() -> TestResult {
    // The KNOWN_CODE_EXPECTATIONS table is the canonical regression
    // guard; duplicate rows would make a category change ambiguous.
    let mut seen: Vec<&str> = Vec::new();
    for (code, _, _) in KNOWN_CODE_EXPECTATIONS {
        if seen.contains(code) {
            return Err(format!(
                "KNOWN_CODE_EXPECTATIONS has duplicate row for `{code}` — pick one canonical category",
            ));
        }
        seen.push(code);
    }
    Ok(())
}
