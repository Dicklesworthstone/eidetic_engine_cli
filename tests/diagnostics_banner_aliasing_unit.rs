//! E2 — `degraded_context` meta-code regression guard (bd-17c65.5.2).
//!
//! The bead spec permanently deleted the `degraded_context` meta-banner —
//! a summary-prose code emitted whenever ANY underlying degraded signal
//! fired. The summary duplicated information already present in
//! `data.degraded[]` and trained agents to ignore the banner. Per the
//! "Granular over coarse" design note, this code is GONE.
//!
//! This test is the canonical regression guard. It uses two layers of
//! defense:
//!
//!   1. **Source-level check** (`source_has_no_degraded_context_emission`):
//!      compiles `src/pack/mod.rs` and `src/output/mod.rs` source via
//!      `include_str!` and asserts no literal `code: "degraded_context"`
//!      construction site remains. This catches the regression at the
//!      narrowest possible point — the exact textual pattern that
//!      represents emission.
//!   2. **Behavioral check** (`degraded_context_string_does_not_appear_in_known_codes`):
//!      the categorization unit test sibling
//!      (`tests/diagnostics_banner_categorization_unit.rs`) enumerates
//!      every documented code; this test asserts `degraded_context` is
//!      NOT among them, so it gets the conservative `AffectsThisResponse`
//!      default if a future bug accidentally reintroduces the string —
//!      visible to the agent rather than silent — but the source-level
//!      gate above prevents the bug from compiling in the first place.
//!
//! Whenever this test fires, fix the impl, not the test. The bead's
//! design says `degraded_context` is permanently retired; if a future
//! contributor genuinely needs a meta-summary, file a new bead with
//! a structured replacement (e.g. `advisoryBanner.summary` as a
//! distinct envelope field) rather than re-adding the legacy code.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::pack::{DegradedCategory, category_for_code};

type TestResult = Result<(), String>;

/// Source-level invariant: no `code: "degraded_context"` emission site
/// survives in `src/pack/mod.rs`. The string MAY appear in comments
/// (as historical reference) but NEVER as a struct field assignment.
#[test]
fn source_has_no_degraded_context_emission_in_pack() -> TestResult {
    let source = include_str!("../src/pack/mod.rs");
    // The forbidden patterns are the literal struct-field assignments
    // that constructed the meta-summary. Comments and docstrings may
    // still mention "degraded_context" for historical context — those
    // are allowed.
    let forbidden_patterns = [
        "code: \"degraded_context\",",
        "code: \"degraded_context\"\n",
        "&\"degraded_context\",",
    ];
    for pattern in forbidden_patterns {
        if source.contains(pattern) {
            let lineno = source
                .lines()
                .position(|line| line.contains(pattern))
                .map(|i| i + 1)
                .unwrap_or(0);
            return Err(format!(
                "src/pack/mod.rs:~{lineno} contains forbidden emission pattern {pattern:?} — \
                 the `degraded_context` meta-code was deleted per bd-17c65.5.2 (E2). \
                 Per-signal information already surfaces in data.degraded[]; do not \
                 re-introduce the meta-summary."
            ));
        }
    }
    Ok(())
}

/// Same invariant for the output render module — the render layer
/// must not construct or emit the legacy code either.
#[test]
fn source_has_no_degraded_context_emission_in_output() -> TestResult {
    let source = include_str!("../src/output/mod.rs");
    let forbidden_patterns = [
        "code: \"degraded_context\",",
        "code: \"degraded_context\"\n",
        "&\"degraded_context\",",
    ];
    for pattern in forbidden_patterns {
        if source.contains(pattern) {
            let lineno = source
                .lines()
                .position(|line| line.contains(pattern))
                .map(|i| i + 1)
                .unwrap_or(0);
            return Err(format!(
                "src/output/mod.rs:~{lineno} contains forbidden emission pattern {pattern:?} — \
                 see bd-17c65.5.2 (E2); the `degraded_context` meta-code is permanently retired."
            ));
        }
    }
    Ok(())
}

/// Behavioral fallback: even if a future regression somehow emits
/// `degraded_context` (e.g. via a string built at runtime that the
/// source-level check can't see), the categorization function falls
/// through to `AffectsThisResponse` — the signal IS visible to the
/// agent rather than silently filtered. This is the safe-default
/// contract.
#[test]
fn degraded_context_falls_through_to_default_category() -> TestResult {
    // The legacy meta-code is NOT explicitly mapped, so it gets the
    // default `AffectsThisResponse`. A runtime-constructed signal
    // with this code would surface to the agent (visible) — never
    // get silently dropped.
    let category = category_for_code("degraded_context");
    if category != DegradedCategory::AffectsThisResponse {
        return Err(format!(
            "category_for_code(\"degraded_context\") = {:?}; \
             must default to AffectsThisResponse so a runtime-leaked emission \
             remains visible to agents rather than silently filtered.",
            category,
        ));
    }
    Ok(())
}

/// Belt-and-suspenders: confirm the legacy code does not appear in
/// either of the two known canonical mappings (build-time or
/// workspace-state). If a future code review accidentally maps
/// `degraded_context` to a non-default category, this catches it.
#[test]
fn degraded_context_is_not_mapped_to_filtered_category() -> TestResult {
    let cat = category_for_code("degraded_context");
    if cat == DegradedCategory::BuildTimeFeatureGap {
        return Err(
            "`degraded_context` must not be mapped to BuildTimeFeatureGap — the legacy meta-code is retired, not categorized".to_string(),
        );
    }
    if cat == DegradedCategory::WorkspaceStateNotPerResponse {
        return Err(
            "`degraded_context` must not be mapped to WorkspaceStateNotPerResponse — the legacy meta-code is retired, not categorized".to_string(),
        );
    }
    Ok(())
}
