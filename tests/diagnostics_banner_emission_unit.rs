//! E2 — degraded[] emission filter behavior (bd-17c65.5.2).
//!
//! Asserts the per-response filter that consumers see:
//!
//!   1. Default emission keeps only `AffectsThisResponse` signals. The
//!      build-time and workspace-state codes are dropped from the
//!      response payload — an agent reading `degraded: []` can infer
//!      everything worked.
//!   2. With `include_non_affecting_degradations = true` (the
//!      `--include-non-affecting-degradations` CLI flag), every
//!      signal surfaces regardless of category. This restores the
//!      pre-E2 verbose behavior for diagnostic walkthroughs.
//!   3. Order is preserved through the filter — the relative ordering
//!      of surviving signals matches their input order so downstream
//!      consumers can rely on stable iteration.
//!
//! The filter is a pure `iter().filter(...)` over `Vec<
//! ContextResponseDegradation>` driven by `category().included_by_default()`
//! (default) or unconditionally (verbose). These tests exercise that
//! predicate at the data-model layer so they stay fast and remain
//! decoupled from the live retrieval pipeline. The wire-level binding
//! is covered by `tests/snapshots/diagnostics_quiet_baseline.snap` and
//! the `scripts/e2e_overhaul/diagnostics_honesty.sh` driver.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::pack::{ContextResponseDegradation, ContextResponseSeverity};

type TestResult = Result<(), String>;

fn make_signal(code: &str) -> ContextResponseDegradation {
    ContextResponseDegradation::new(
        code,
        ContextResponseSeverity::Medium,
        format!("synthetic message for {code}"),
        None,
    )
    .expect("synthetic degradation must validate (non-empty code/message)")
}

/// Emulate the in-process filter the renderer applies: keep every
/// signal whose category is `included_by_default`, OR unconditionally
/// when `include_all` is true. Returns codes in the surviving order.
fn apply_filter(
    signals: &[ContextResponseDegradation],
    include_all: bool,
) -> Vec<String> {
    signals
        .iter()
        .filter(|d| include_all || d.category().included_by_default())
        .map(|d| d.code.clone())
        .collect()
}

#[test]
fn default_emission_drops_workspace_state_signals() -> TestResult {
    let signals = vec![
        make_signal("no_relevant_results"),
        make_signal("search_index_stale"),
        make_signal("weak_query_recall"),
    ];
    let kept = apply_filter(&signals, false);
    let expected = vec![
        "no_relevant_results".to_string(),
        "weak_query_recall".to_string(),
    ];
    if kept != expected {
        return Err(format!(
            "default emission should drop search_index_stale; kept = {kept:?}",
        ));
    }
    Ok(())
}

#[test]
fn default_emission_drops_build_time_feature_gaps() -> TestResult {
    let signals = vec![
        make_signal("duplicates_collapsed"),
        make_signal("graph_snapshot_unimplemented"),
        make_signal("mcp_feature_disabled"),
    ];
    let kept = apply_filter(&signals, false);
    if kept != vec!["duplicates_collapsed".to_string()] {
        return Err(format!(
            "default emission should drop build-time feature gaps; kept = {kept:?}",
        ));
    }
    Ok(())
}

#[test]
fn default_emission_returns_empty_when_all_signals_are_non_affecting() -> TestResult {
    // The clean-baseline invariant: if every signal is workspace-state
    // or build-time, the per-response degraded[] is EMPTY. An agent
    // seeing `degraded: []` infers the response was successful.
    let signals = vec![
        make_signal("search_index_stale"),
        make_signal("graph_snapshot_missing"),
        make_signal("mcp_unavailable"),
    ];
    let kept = apply_filter(&signals, false);
    if !kept.is_empty() {
        return Err(format!(
            "all-non-affecting input should produce empty degraded[]; got {kept:?}",
        ));
    }
    Ok(())
}

#[test]
fn verbose_emission_surfaces_every_signal_regardless_of_category() -> TestResult {
    let signals = vec![
        make_signal("no_relevant_results"),
        make_signal("search_index_stale"),
        make_signal("graph_snapshot_unimplemented"),
    ];
    let kept = apply_filter(&signals, true);
    let expected = vec![
        "no_relevant_results".to_string(),
        "search_index_stale".to_string(),
        "graph_snapshot_unimplemented".to_string(),
    ];
    if kept != expected {
        return Err(format!(
            "verbose emission should keep ALL signals in input order; got {kept:?}",
        ));
    }
    Ok(())
}

#[test]
fn filter_preserves_relative_input_order_after_drops() -> TestResult {
    // The filter is a partition over the iterator, not a sort. The
    // surviving order is the input order. A future maintainer who
    // refactors the filter into a more complex pipeline (priority
    // weighting, deduplication, etc.) must not silently reorder.
    let signals = vec![
        make_signal("search_index_stale"),         // 0 → drop
        make_signal("no_relevant_results"),         // 1 → keep
        make_signal("graph_snapshot_unimplemented"), // 2 → drop
        make_signal("duplicates_collapsed"),        // 3 → keep
        make_signal("cass_unavailable"),            // 4 → drop
        make_signal("weak_query_recall"),           // 5 → keep
    ];
    let kept = apply_filter(&signals, false);
    let expected = vec![
        "no_relevant_results".to_string(),
        "duplicates_collapsed".to_string(),
        "weak_query_recall".to_string(),
    ];
    if kept != expected {
        return Err(format!(
            "filter must preserve relative input order; expected {expected:?}, got {kept:?}",
        ));
    }
    Ok(())
}

#[test]
fn empty_input_yields_empty_output_in_both_modes() -> TestResult {
    let signals: Vec<ContextResponseDegradation> = Vec::new();
    if !apply_filter(&signals, false).is_empty() {
        return Err("empty input default mode must produce empty output".to_string());
    }
    if !apply_filter(&signals, true).is_empty() {
        return Err("empty input verbose mode must produce empty output".to_string());
    }
    Ok(())
}

#[test]
fn unknown_code_is_kept_under_default_mode() -> TestResult {
    // The conservative-default contract: an unknown code is treated
    // as AffectsThisResponse and kept under the default filter. This
    // is the safety property the bead spec'd — new codes that have
    // not been categorized yet still reach the agent.
    let signals = vec![
        make_signal("future_unknown_code_42"),
        make_signal("search_index_stale"), // known workspace-state — dropped
    ];
    let kept = apply_filter(&signals, false);
    if kept != vec!["future_unknown_code_42".to_string()] {
        return Err(format!(
            "unknown code must be kept by default; got {kept:?}",
        ));
    }
    Ok(())
}
