#!/usr/bin/env bash
# bd-7lvbg.4 — output-token governor E2E (ADR 0063, real binary).
#
# Seeds a 500-memory deterministic corpus through `ee remember --batch
# --stdin` (one process, generator-stable content), then proves the
# governor contract across the wired surfaces:
#
#   * Ceiling sweep (100 / 500 / 2000 / none) over search, memory list,
#     insights, curate candidates, and schema list: every response is
#     valid JSON; under a ceiling the stamped meta.tokensEstimated is
#     <= the ceiling unless the response honestly fails closed with
#     output_budget_unsatisfiable; with no ceiling the zero-cost path
#     leaves the envelope unstamped.
#   * audit timeline is a top-level report, not an ee.response.v2
#     envelope: the governor must pass it through unstamped even when a
#     ceiling is set (its --cursor lane is query-level pagination on
#     the shared ee.cursor.v1 codec).
#   * Cursor pagination drains memory list and search to exhaustion
#     with EXACT counts: concatenated page ids equal the ungoverned id
#     sequence (no duplicates, no gaps, order preserved).
#   * A generation advance mid-pagination (one extra remember between
#     pages) turns the next resume into an EMPTY cursor_stale page —
#     never a silent mix of two generations.
#   * EE_MAX_OUTPUT_TOKENS env is byte-equivalent to the
#     --max-output-tokens flag.
#
# Every step emits an ee.test_event.v1 event (step, surface, ceiling,
# estimated, items, cursor presence, elapsed ms) via the shared harness
# logger; assertions accumulate and harness_summary owns the exit code.
#
# NOTE: no `set -e` — assert_* accumulate pass/fail and harness_summary
# decides the exit code, so a single failing assert must not abort the
# run before the summary.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "output_governor"

if ! command -v jq >/dev/null 2>&1; then
    echo "output_governor: jq is required" >&2
    exit 3
fi

# ee_json <args...> — run ee, tolerate nonzero exit (assertions inspect output).
ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }

now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

# 500 per the bd-7lvbg.4 spec; constrained lanes may dial it down (the
# drain/staleness assertions are corpus-size independent).
CORPUS_SIZE="${EE_GOVERNOR_E2E_CORPUS:-500}"

with_temp_workspace WS

step "init workspace + seed ${CORPUS_SIZE}-memory deterministic corpus (remember --batch)"
init_out="$(ee_json --workspace "$WS" init --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

seed_start="$(now_ms)"
seed_out="$(
    awk -v n="$CORPUS_SIZE" 'BEGIN {
        for (i = 0; i < n; i++) {
            printf "{\"content\":\"Governor e2e corpus row %04d: deterministic filler about the release workflow, clippy gating, and index posture conventions.\",\"level\":\"semantic\",\"kind\":\"fact\",\"tags\":[\"governor\",\"e2e\"]}\n", i
        }
    }' | ee_json --workspace "$WS" remember --batch --stdin --json
)"
seed_ms=$(( $(now_ms) - seed_start ))
assert_jq "$seed_out" '.success == true' "batch remember succeeds"
assert_json "$seed_out" '.data.storedCount' "$CORPUS_SIZE" "batch stored exactly ${CORPUS_SIZE} rows"
log_event "governor_seed" step seed surface remember corpus "$CORPUS_SIZE" ms "$seed_ms"

# ---------------------------------------------------------------------------
# Ceiling sweep across the wired envelope surfaces.
#
# sweep_surface <surface-label> <items-jq-path> <args...>
#   For each ceiling in 100/500/2000/none: run, assert valid JSON, then
#   assert the governed-output invariant (estimate <= ceiling OR an
#   honest unsatisfiable fail-closed) or the unstamped zero-cost path.
# ---------------------------------------------------------------------------
sweep_surface() {
    local surface="$1" items_path="$2"
    shift 2
    local ceiling out call_ms start estimated items cursor_present
    for ceiling in 100 500 2000 none; do
        start="$(now_ms)"
        if [ "$ceiling" = "none" ]; then
            out="$(ee_json --workspace "$WS" "$@" --json)"
        else
            out="$(ee_json --workspace "$WS" "$@" --max-output-tokens "$ceiling" --json)"
        fi
        call_ms=$(( $(now_ms) - start ))
        assert_jq "$out" 'true' "${surface} ceiling=${ceiling}: output is valid JSON"
        if [ "$ceiling" = "none" ]; then
            assert_jq "$out" '(.meta.tokensEstimated // null) == null' \
                "${surface} ceiling=none: zero-cost path leaves envelope unstamped"
        else
            assert_jq "$out" \
                "((.meta.tokensEstimated // 0) <= ${ceiling}) or ([(.degraded[]?, .data.degraded[]?) | .code] | index(\"output_budget_unsatisfiable\") != null)" \
                "${surface} ceiling=${ceiling}: estimate honors ceiling or fails closed"
        fi
        estimated="$(printf '%s' "$out" | jq -r '.meta.tokensEstimated // "none"' 2>/dev/null || echo parse_error)"
        items="$(printf '%s' "$out" | jq -r "${items_path} | length" 2>/dev/null || echo 0)"
        cursor_present="$(printf '%s' "$out" | jq -r '[(.degraded[]?, .data.degraded[]?) | .details.continuationCursor // empty] | length > 0' 2>/dev/null || echo false)"
        log_event "governor_sweep" step sweep surface "$surface" ceiling "$ceiling" \
            estimated "$estimated" items "$items" cursor "$cursor_present" ms "$call_ms"
    done
}

step "ceiling sweep: search"
sweep_surface "search" '.data.results // []' search "governor e2e corpus row" --limit 50

step "ceiling sweep: memory list"
sweep_surface "memory_list" '.data.memories // []' memory list --limit "$CORPUS_SIZE"

step "ceiling sweep: insights"
sweep_surface "insights" '[.data.sections[]?.items[]?] // []' insights

step "ceiling sweep: curate candidates"
sweep_surface "curate_candidates" '.data.candidates // []' curate candidates

step "ceiling sweep: schema list"
sweep_surface "schema_list" '.data.schemas // []' schema list

step "audit timeline is a pass-through report (non-envelope), even under a ceiling"
at_start="$(now_ms)"
at_out="$(ee_json --workspace "$WS" audit timeline --limit 5 --max-output-tokens 100 --json)"
at_ms=$(( $(now_ms) - at_start ))
assert_jq "$at_out" 'true' "audit timeline ceiling=100: output is valid JSON"
assert_jq "$at_out" '.schema == "ee.audit.timeline.v1"' "audit timeline keeps its report schema"
assert_jq "$at_out" '(.meta.tokensEstimated // null) == null' \
    "audit timeline is never stamped by the envelope governor (pass-through)"
log_event "governor_sweep" step sweep surface "audit_timeline" ceiling 100 \
    estimated none items "$(printf '%s' "$at_out" | jq -r '.entries | length')" cursor false ms "$at_ms"

# ---------------------------------------------------------------------------
# Cursor pagination drains with exact counts.
#
# drain_surface <surface-label> <items-array-path> <id-field> <ceiling> <args...>
#   Compares the concatenated drained ids against the ungoverned id
#   sequence byte-for-byte: exact partition, order preserved.
# ---------------------------------------------------------------------------
drain_surface() {
    local surface="$1" items_path="$2" id_field="$3" ceiling="$4"
    shift 4
    local full_ids out page_ids cursor page drained="" page_start page_ms

    out="$(ee_json --workspace "$WS" "$@" --json)"
    full_ids="$(printf '%s' "$out" | jq -r "${items_path}[].${id_field}" 2>/dev/null)"
    local full_count
    full_count="$(printf '%s\n' "$full_ids" | grep -c . || true)"
    assert_eq "$([ "$full_count" -gt 0 ] && echo nonempty || echo empty)" "nonempty" \
        "${surface}: ungoverned baseline returns rows (got ${full_count})"

    cursor=""
    for page in $(seq 0 399); do
        page_start="$(now_ms)"
        if [ -z "$cursor" ]; then
            out="$(ee_json --workspace "$WS" "$@" --max-output-tokens "$ceiling" --json)"
        else
            out="$(ee_json --workspace "$WS" "$@" --max-output-tokens "$ceiling" --cursor "$cursor" --json)"
        fi
        page_ms=$(( $(now_ms) - page_start ))
        assert_jq "$out" 'true' "${surface} drain page ${page}: valid JSON"
        if printf '%s' "$out" | jq -e '[(.degraded[]?, .data.degraded[]?) | .code] | index("output_budget_unsatisfiable") != null' >/dev/null 2>&1; then
            if [ -z "$cursor" ]; then
                _harness_fail "${surface} drain: page 0 failed closed (output_budget_unsatisfiable) — ceiling ${ceiling} cannot hold the envelope minimum for this surface; raise the drain ceiling"
            else
                _harness_fail "${surface} drain page ${page}: output_budget_unsatisfiable mid-sequence — a remainder page must never fail closed when page 0 fit"
            fi
            break
        fi
        if [ -n "$cursor" ]; then
            assert_jq "$out" '[(.degraded[]?, .data.degraded[]?) | .code] | (index("cursor_invalid") == null) and (index("cursor_stale") == null)' \
                "${surface} drain page ${page}: own cursor accepted"
        fi
        page_ids="$(printf '%s' "$out" | jq -r "${items_path}[].${id_field}" 2>/dev/null)"
        if [ -n "$page_ids" ]; then
            drained="${drained}${page_ids}"$'\n'
        fi
        log_event "governor_drain" step drain surface "$surface" ceiling "$ceiling" \
            page "$page" items "$(printf '%s\n' "$page_ids" | grep -c . || true)" \
            cursor "$([ -n "$cursor" ] && echo resume || echo fresh)" ms "$page_ms"
        cursor="$(printf '%s' "$out" | jq -r '[(.degraded[]?, .data.degraded[]?) | select(.code == "output_truncated_budget") | .details.continuationCursor][0] // empty' 2>/dev/null)"
        if [ -z "$cursor" ]; then
            break
        fi
    done
    assert_eq "$([ -z "$cursor" ] && echo terminated || echo unterminated)" "terminated" \
        "${surface}: drain terminated within 400 pages"

    local drained_count
    drained_count="$(printf '%s' "$drained" | grep -c . || true)"
    assert_eq "$drained_count" "$full_count" \
        "${surface}: drained id count equals ungoverned count"
    if [ "$(printf '%s' "$drained")" = "$full_ids" ]; then
        _harness_pass "${surface}: drained pages partition the full id sequence exactly"
    else
        _harness_fail "${surface}: drained id sequence diverged from the ungoverned baseline"
    fi
    log_event "governor_drain_summary" step drain surface "$surface" ceiling "$ceiling" \
        items "$drained_count" cursor exhausted ms 0
}

step "cursor drain with exact counts: memory list (${CORPUS_SIZE} rows)"
drain_surface "memory_list" '.data.memories' 'id' 2000 memory list --limit "$CORPUS_SIZE"

# Search result elements are heavy (content + score breakdowns, ~700+
# estimated tokens each), so the drain ceiling must comfortably hold the
# shell + one element + the cursor entry.
step "cursor drain with exact counts: search (limit 50)"
drain_surface "search" '.data.results' 'docId' 2500 search "governor e2e corpus row" --limit 50

step "generation advance mid-pagination yields an EMPTY cursor_stale page"
page1="$(ee_json --workspace "$WS" memory list --limit "$CORPUS_SIZE" --max-output-tokens 2000 --json)"
stale_cursor="$(printf '%s' "$page1" | jq -r '[(.degraded[]?, .data.degraded[]?) | select(.code == "output_truncated_budget") | .details.continuationCursor][0] // empty')"
assert_eq "$([ -n "$stale_cursor" ] && echo cursor || echo none)" "cursor" \
    "page 1 under a 2000-token ceiling offers a continuation cursor"
bump_out="$(ee_json --workspace "$WS" remember \
    "Governor e2e generation bump row written between pages." \
    --level semantic --kind fact --tags governor,e2e --json)"
assert_jq "$bump_out" '.success == true' "mid-pagination remember succeeds (generation advances)"
stale_out="$(ee_json --workspace "$WS" memory list --limit "$CORPUS_SIZE" --max-output-tokens 2000 --cursor "$stale_cursor" --json)"
assert_jq "$stale_out" '[(.degraded[]?, .data.degraded[]?) | .code] | index("cursor_stale") != null' \
    "resume after a write reports cursor_stale"
assert_jq "$stale_out" '(.data.memories // []) | length == 0' \
    "stale page is EMPTY — never a silent restart or generation mix"
assert_jq "$stale_out" '[(.degraded[]?, .data.degraded[]?) | .details.continuationCursor // empty] | length == 0' \
    "stale page offers no continuation cursor"
log_event "governor_stale" step stale surface "memory_list" ceiling 2000 \
    estimated none items 0 cursor rejected_stale ms 0

# Byte-equality across two separate invocations needs a surface whose
# payload carries no run-varying diagnostics; memory list is pinned
# byte-deterministic cross-process by the J7 harness. Search's own
# data.degraded details can vary between invocations, so it is the
# wrong surface for this specific assertion.
step "EE_MAX_OUTPUT_TOKENS env is byte-equivalent to --max-output-tokens"
flag_out="$(ee_json --workspace "$WS" memory list --limit "$CORPUS_SIZE" --max-output-tokens 500 --json)"
env_out="$(EE_MAX_OUTPUT_TOKENS=500 ee_json --workspace "$WS" memory list --limit "$CORPUS_SIZE" --json)"
assert_jq "$flag_out" 'true' "flag-governed memory list is valid JSON"
assert_jq "$flag_out" '(.meta.tokensEstimated // null) != null' \
    "flag-governed memory list is stamped (governor engaged)"
if [ "$flag_out" = "$env_out" ]; then
    _harness_pass "env-governed output is byte-identical to flag-governed output"
else
    printf '%s' "$flag_out" > "$LOG_DIR/env_equivalence_flag.json"
    printf '%s' "$env_out" > "$LOG_DIR/env_equivalence_env.json"
    first_diff="$(diff <(printf '%s' "$flag_out" | jq -S . 2>/dev/null) \
        <(printf '%s' "$env_out" | jq -S . 2>/dev/null) 2>/dev/null | head -6 | tr '\n' ' ')"
    _harness_fail "EE_MAX_OUTPUT_TOKENS=500 output diverged from --max-output-tokens 500 output; payloads saved to $LOG_DIR/env_equivalence_{flag,env}.json; first diff: ${first_diff:-binary-or-jq-failure}"
fi
log_event "governor_env_equivalence" step env surface "memory_list" ceiling 500 \
    estimated "$(printf '%s' "$flag_out" | jq -r '.meta.tokensEstimated // "none"')" \
    items "$(printf '%s' "$flag_out" | jq -r '.data.memories | length')" \
    cursor "$(printf '%s' "$flag_out" | jq -r '[(.degraded[]?, .data.degraded[]?) | .details.continuationCursor // empty] | length > 0')" \
    ms 0

end_temp_workspace
harness_summary
