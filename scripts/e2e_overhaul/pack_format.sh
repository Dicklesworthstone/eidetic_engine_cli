#!/usr/bin/env bash
# J3 — Epic A: pack format densification e2e driver.
#
# Drives `ee context` against the 2026-05-10 reference corpus and asserts the
# pack envelope is dense, honest, and stable. Each assertion maps to a bead
# in epic A. When future A-surface gaps are discovered, add explicit assertions
# or a fresh follow-up bead rather than leaving stale TODO notes in this driver.
#
# Shipped (real assertions):  A1, A2, A3, A4, A5, A6, A7, A8, A9
# Covered elsewhere:          A11 (`tests/contracts/context_show_persisted_pack.rs`)
#
# Usage:
#   scripts/e2e_overhaul/pack_format.sh
#
# Env:
#   EE_BINARY          path to ee binary (default: target/release/ee)
#   EE_TEST_LOG_PATH   if set, emits J1 events to this file

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_A_pack_format"

# Seed corpus (with pre-overhaul tolerance). The corpus contains 15 memories,
# 11 of which seed successfully under pre-C1/C3 binaries.
seed_corpus

# ------------------------------------------------------------
# Run ee context against the seeded workspace.
# ------------------------------------------------------------
PACK_JSON=$(ee_workspace context "prepare release v0.2.0" --max-tokens 1000 --json || true)

if ! printf '%s' "$PACK_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_note "pack_json_unparseable bytes=${#PACK_JSON}"
    e2e_log_assert_eq "false" "true" "pack_json_parses"
    exit 0
fi

# ------------------------------------------------------------
# A3 (shipped) — per-item `why` is a 1-line actionable string, not a 350-char
# math identity tutorial.
# ------------------------------------------------------------
WHY_FIRST=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.items[0].why // ""' 2>/dev/null || true)
WHY_LEN=${#WHY_FIRST}
if [ "$WHY_LEN" -gt 0 ]; then
    # A3 acceptance: why is short and contains the literal "matched" verb.
    e2e_log_assert_num "$WHY_LEN" -le 200 "a3_per_item_why_short"
    if printf '%s' "$WHY_FIRST" | grep -qE 'matched|via'; then
        e2e_log_assert_eq "true" "true" "a3_per_item_why_is_actionable"
    else
        e2e_log_assert_eq "lacks 'matched'/'via' verbs" "actionable" "a3_per_item_why_is_actionable"
    fi
else
    # No items returned — corpus seed may have produced an empty pack on this
    # binary. Note it instead of asserting against missing data.
    e2e_log_note "a3_pack_has_no_items skip=true"
fi

# ------------------------------------------------------------
# A7 (shipped) — markdown render uses contiguous 1..N item numbers.
# ------------------------------------------------------------
PACK_MD=$(ee_workspace context "prepare release v0.2.0" --max-tokens 1000 --format markdown || true)
MAX_INDEX=$(printf '%s' "$PACK_MD" | grep -oE '^### [0-9]+\.' | awk '{print $2}' | tr -d '.' | sort -n | tail -1)
INDEX_COUNT=$(printf '%s' "$PACK_MD" | grep -cE '^### [0-9]+\.' || true)
if [ -n "$MAX_INDEX" ] && [ -n "$INDEX_COUNT" ] && [ "$INDEX_COUNT" -gt 0 ]; then
    e2e_log_assert_eq "$MAX_INDEX" "$INDEX_COUNT" "a7_contiguous_item_indices"
else
    e2e_log_note "a7_no_indexed_items_in_markdown skip=true"
fi

# ------------------------------------------------------------
# A9 (shipped) — pack persistence is no longer reported as
# `context_pack_persist_failed` in degraded[] when the workspace path differs
# from its canonical form (the bug A9 fixed). We can't easily induce the
# canonical/raw path mismatch from a script, so assert the absence in the
# nominal path.
# ------------------------------------------------------------
PERSIST_FAIL_COUNT=$(printf '%s' "$PACK_JSON" \
    | jq '[.data.degraded[]?.code // empty] | map(select(. == "context_pack_persist_failed")) | length' 2>/dev/null \
    || echo 0)
e2e_log_assert_eq "$PERSIST_FAIL_COUNT" "0" "a9_no_pack_persist_failure_for_canonical_path"

# ------------------------------------------------------------
# A1 (shipped) — canonical items[] consolidates per-item data from the
# legacy selectedItems / steps / provenance-footer entries. The aggregate
# selectionAudit and provenanceFooter summaries remain, but they no longer
# carry parallel per-item arrays by default.
# ------------------------------------------------------------
HAS_SELECTION_AUDIT=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack | has("selectionAudit")' 2>/dev/null || echo false)
HAS_SELECTION_CERTIFICATE=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack | has("selectionCertificate")' 2>/dev/null || echo false)
AUDIT_HAS_SELECTED_ITEMS=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.selectionAudit | has("selectedItems") or has("selected_items")' 2>/dev/null || echo false)
AUDIT_HAS_STEPS=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.selectionAudit | has("steps")' 2>/dev/null || echo false)
FOOTER_HAS_ENTRIES=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.provenanceFooter | has("entries")' 2>/dev/null || echo false)
e2e_log_assert_eq "$HAS_SELECTION_AUDIT" "true" "a1_selection_audit_summary_present"
e2e_log_assert_eq "$HAS_SELECTION_CERTIFICATE" "false" "a1_legacy_selection_certificate_omitted"
e2e_log_assert_eq "$AUDIT_HAS_SELECTED_ITEMS" "false" "a1_no_parallel_selected_items"
e2e_log_assert_eq "$AUDIT_HAS_STEPS" "false" "a1_no_parallel_selection_steps"
e2e_log_assert_eq "$FOOTER_HAS_ENTRIES" "false" "a1_no_parallel_provenance_footer_entries"

ITEM_COUNT=$(printf '%s' "$PACK_JSON" | jq '.data.pack.items | length' 2>/dev/null || echo 0)
if [ "$ITEM_COUNT" -gt 0 ]; then
    FIRST_HAS_TOKEN_COST=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.items[0] | has("tokenCost")' 2>/dev/null || echo false)
    FIRST_HAS_FEASIBLE=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.items[0] | has("feasible")' 2>/dev/null || echo false)
    FIRST_HAS_COVERED_FEATURES=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.items[0] | has("coveredFeatures")' 2>/dev/null || echo false)
    FIRST_HAS_SOURCE_INDEX=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.items[0] | has("sourceIndex")' 2>/dev/null || echo false)
    e2e_log_assert_eq "$FIRST_HAS_TOKEN_COST" "true" "a1_items_inline_token_cost"
    e2e_log_assert_eq "$FIRST_HAS_FEASIBLE" "true" "a1_items_inline_feasible"
    e2e_log_assert_eq "$FIRST_HAS_COVERED_FEATURES" "true" "a1_items_inline_covered_features"
    e2e_log_assert_eq "$FIRST_HAS_SOURCE_INDEX" "true" "a1_items_inline_source_index"
else
    e2e_log_note "a1_no_items_to_check_inline_fields skip=true"
fi

# ------------------------------------------------------------
# A2 (shipped) — algorithm description is emitted once at pack.meta.algorithm
# instead of repeated in per-step structures.
# ------------------------------------------------------------
META_ALGORITHM_ID=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.meta.algorithm.algorithmId // ""' 2>/dev/null || true)
AUDIT_ALGORITHM_ID=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.selectionAudit.algorithmId // ""' 2>/dev/null || true)
e2e_log_assert_eq "$META_ALGORITHM_ID" "$AUDIT_ALGORITHM_ID" "a2_algorithm_metadata_matches_audit"
WHY_HAS_UNIT_SCORE=$(printf '%s' "$WHY_FIRST" | grep -c 'unit_score' || true)
e2e_log_assert_eq "$WHY_HAS_UNIT_SCORE" "0" "a2_no_per_item_formula_boilerplate"

# ------------------------------------------------------------
# A4 (shipped) — pack.text embeds the canonical Markdown render in JSON.
# ------------------------------------------------------------
HAS_PACK_TEXT=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack | has("text")' 2>/dev/null || echo false)
PACK_TEXT_HAS_HEADER=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.text // "" | startswith("# Context Pack:")' 2>/dev/null || echo false)
e2e_log_assert_eq "$HAS_PACK_TEXT" "true" "a4_pack_text_field_present"
e2e_log_assert_eq "$PACK_TEXT_HAS_HEADER" "true" "a4_pack_text_is_markdown_fragment"

# ------------------------------------------------------------
# A5 (shipped) — omitted/rejected candidates are surfaced through the unified
# pack.skipped[] list, not selectionAudit.rejectedFrontier[].
# ------------------------------------------------------------
HAS_SKIPPED=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack | has("skipped")' 2>/dev/null || echo false)
HAS_REJECTED_FRONTIER=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.selectionAudit | has("rejectedFrontier")' 2>/dev/null || echo false)
e2e_log_assert_eq "$HAS_SKIPPED" "true" "a5_unified_skipped_array_present"
e2e_log_assert_eq "$HAS_REJECTED_FRONTIER" "false" "a5_no_rejected_frontier_parallel_list"

# ------------------------------------------------------------
# A6 (shipped) — coverage fill is part of the pack algorithm metadata and each
# item records whether it entered during strict MMR or coverage-fill selection.
# ------------------------------------------------------------
HAS_COVERAGE_FILL_COUNT=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.meta | has("coverageFillCount")' 2>/dev/null || echo false)
HAS_SELECTED_IN=$(printf '%s' "$PACK_JSON" | jq -r '([.data.pack.items[]? | has("selectedIn")] | all)' 2>/dev/null || echo false)
e2e_log_assert_eq "$HAS_COVERAGE_FILL_COUNT" "true" "a6_coverage_fill_count_reported"
e2e_log_assert_eq "$HAS_SELECTED_IN" "true" "a6_items_report_selection_phase"

# A8 — pack output profiles and opt-out flags.
LEAN_JSON=$(ee_workspace context "prepare release v0.2.0" --max-tokens 1000 --pack-profile lean --json || true)
STANDARD_JSON=$(ee_workspace context "prepare release v0.2.0" --max-tokens 1000 --pack-profile standard --json || true)
VERBOSE_JSON=$(ee_workspace context "prepare release v0.2.0" --max-tokens 1000 --pack-profile verbose --json || true)

if printf '%s' "$LEAN_JSON" | jq . >/dev/null 2>&1 &&
    printf '%s' "$STANDARD_JSON" | jq . >/dev/null 2>&1 &&
    printf '%s' "$VERBOSE_JSON" | jq . >/dev/null 2>&1; then
    LEAN_BYTES=${#LEAN_JSON}
    STANDARD_BYTES=${#STANDARD_JSON}
    VERBOSE_BYTES=${#VERBOSE_JSON}
    e2e_log_note "a8_profile_bytes lean=$LEAN_BYTES standard=$STANDARD_BYTES verbose=$VERBOSE_BYTES"
    e2e_log_assert_num "$LEAN_BYTES" -lt "$STANDARD_BYTES" "a8_lean_smaller_than_standard"
    e2e_log_assert_num "$STANDARD_BYTES" -lt "$VERBOSE_BYTES" "a8_verbose_larger_than_standard"

    LEAN_HAS_TEXT=$(printf '%s' "$LEAN_JSON" | jq -r '.data.pack | has("text")')
    LEAN_HAS_SKIPPED=$(printf '%s' "$LEAN_JSON" | jq -r '.data.pack | has("skipped")')
    LEAN_COVERAGE=$(printf '%s' "$LEAN_JSON" | jq -r '.data.pack.meta.coverageFillCount // 0')
    STANDARD_HAS_TEXT=$(printf '%s' "$STANDARD_JSON" | jq -r '.data.pack | has("text")')
    STANDARD_HAS_SKIPPED=$(printf '%s' "$STANDARD_JSON" | jq -r '.data.pack | has("skipped")')
    VERBOSE_HAS_FORMULA=$(printf '%s' "$VERBOSE_JSON" | jq -r '.data.pack.meta | has("selectionFormula")')
    e2e_log_assert_eq "$LEAN_HAS_TEXT" "false" "a8_lean_omits_rendered_text"
    e2e_log_assert_eq "$LEAN_HAS_SKIPPED" "false" "a8_lean_omits_skipped"
    e2e_log_assert_eq "$LEAN_COVERAGE" "0" "a8_lean_disables_coverage_fill"
    e2e_log_assert_eq "$STANDARD_HAS_TEXT" "true" "a8_standard_includes_rendered_text"
    e2e_log_assert_eq "$STANDARD_HAS_SKIPPED" "true" "a8_standard_includes_skipped"
    e2e_log_assert_eq "$VERBOSE_HAS_FORMULA" "true" "a8_verbose_includes_formula_metadata"

    LEAN_SKIPPED_OVERRIDE=$(ee_workspace context "prepare release v0.2.0" \
        --max-tokens 1000 --pack-profile lean --no-skipped=false --json || true)
    OVERRIDE_HAS_SKIPPED=$(printf '%s' "$LEAN_SKIPPED_OVERRIDE" | jq -r '.data.pack | has("skipped")' 2>/dev/null || echo false)
    e2e_log_assert_eq "$OVERRIDE_HAS_SKIPPED" "true" "a8_lean_no_skipped_false_restores_skipped"
else
    e2e_log_assert_eq "unparseable" "json" "a8_profile_json_parses"
fi

e2e_log_note "a11_context_show_covered_by=tests/contracts/context_show_persisted_pack.rs"
