#!/usr/bin/env bash
# J3 — Epic A: pack format densification e2e driver.
#
# Drives `ee context` against the 2026-05-10 reference corpus and asserts the
# pack envelope is dense, honest, and stable. Each assertion maps to a bead
# in epic A. Assertions for shipped beads are real; assertions for unshipped
# beads are recorded via todo_assert() so the script always reports an honest
# picture of progress without flipping its exit code on known-unimplemented
# surfaces.
#
# Shipped (real assertions):  A3, A7, A8, A9
# Not yet shipped (todo):     A1, A2, A4, A5, A6, A11
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
# A1 (not shipped) — canonical items[] consolidates the four parallel
# structures (selectedItems, items, selectionAudit.steps,
# provenanceFooter.entries) into one. Today these are still distinct, so we
# record TODOs.
# ------------------------------------------------------------
todo_assert "a1_single_canonical_items_array" "bd-17c65.1.1" \
    "Currently has selectedItems[], items[], selectionAudit.steps[], provenanceFooter.entries[] as four parallel structures."

# A4 — pack.text (markdown render embedded in JSON).
todo_assert "a4_pack_text_field_present" "bd-17c65.1.4" \
    "Currently no pack.text in JSON response — agents must call --format markdown separately."
HAS_PACK_TEXT=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack | has("text")' 2>/dev/null || echo false)
e2e_log_note "a4_pack_has_text_field=$HAS_PACK_TEXT"

# A5 — unify omitted[] and rejectedFrontier[] into pack.skipped[].
todo_assert "a5_unified_skipped_array" "bd-17c65.1.6" \
    "Currently omitted[] and rejectedFrontier[] are separate parallel skip lists."

# A2 — collapse per-item math why into shared selectionAudit.formula.
todo_assert "a2_shared_selection_formula_block" "bd-17c65.1.2" \
    "Per-item why no longer contains the math (A3 done), but the shared formula block isn't surfaced yet."

# A6 — pack.budget includes usedTokens, maxTokens, sectionTokens breakdown.
todo_assert "a6_budget_breakdown_by_section" "bd-17c65.1.5" \
    "pack.budget today shows max+used; per-section breakdown not yet emitted."

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

# A11 — pack.advisoryBanner stops being non-empty when nothing is degraded.
todo_assert "a11_advisory_banner_conditional" "bd-17c65.1.10" \
    "Advisory banner currently emits when no issues exist — see E2 for the conditional emission fix."
