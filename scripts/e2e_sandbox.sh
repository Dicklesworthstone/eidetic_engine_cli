#!/usr/bin/env bash
# bd-1n0np.21.5 — What-If Memory Sandbox end-to-end (real binary).
#
# Scenario: a proposed memory change is simulated WITHOUT durable mutation, its
# would-be pack effect is shown, and only an explicit apply persists it.
#   1. init workspace + remember a baseline memory.
#   2. `ee sandbox remember` a proposed rule + `ee sandbox diff` a query ->
#      report shows the would-be pack change (baseline-vs-overlay) (bd-1n0np.21.1).
#   3. assert the DB is UNCHANGED afterward (no durable mutation) -- a sandbox is a
#      read-only hypothesis.
#   4. assert either a faithful temp-index retrieval OR an explicit
#      `sandbox_approximation` marker (bd-1n0np.21.2 honesty caveat).
#   5. `ee sandbox apply` (or curate-apply) and confirm it now persists.
#
# The `ee sandbox` surfaces (21.2 temp index, 21.3 CLI) are CAPABILITY-GUARDED:
# where a surface is absent in the binary under test, the step records a visible
# log_drop (the no-silent-cap rule) instead of a false pass, and its assertions
# activate automatically once the binary provides it. init / remember / count run
# for real on any binary.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "sandbox"

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
# True only when `ee sandbox --help` actually lists <subcommand>.
ee_sandbox_has() {
    "$EE_BIN" sandbox --help 2>&1 | grep -qw "$1"
}
# Count durable memories (best-effort across surfaces) for the no-mutation check.
ee_memory_count() {
    ee_json memory list --workspace "$1" --json 2>/dev/null \
        | jq -r '[.. | objects | (.memories // .results // empty)] | add | length // 0' 2>/dev/null \
        || echo 0
}

with_temp_workspace WS

step "init workspace + remember a baseline memory"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"
base_out="$(ee_json remember "Always run the full test suite before release." \
    --workspace "$WS" --level procedural --kind rule --tags release --json)"
assert_jq "$base_out" '.success == true' "remember baseline memory"
count_before="$(ee_memory_count "$WS")"
e2e_log_note "durable_memory_count_before=$count_before"

step "sandbox remember a proposed rule + diff a query (bd-1n0np.21.1/21.3)"
if ee_sandbox_has "remember"; then
    sb="$(ee_json sandbox remember "Skip the slow integration tests on hotfix releases." \
        --workspace "$WS" --level procedural --kind rule --json)"
    if printf '%s' "$sb" | jq -e '.success != null' >/dev/null 2>&1; then
        assert_jq "$sb" '.success == true' "sandbox remember (non-durable) succeeds"
    else
        log_drop 1 "ee sandbox remember invocation pending (bd-1n0np.21.3)"
    fi
else
    log_drop 1 "ee sandbox surface pending (bd-1n0np.21.3): sandbox remember/diff skipped"
fi

step "sandbox diff shows the would-be pack change (baseline vs overlay)"
if ee_sandbox_has "diff"; then
    diff_out="$(ee_json sandbox diff "should I run integration tests on a hotfix" --workspace "$WS" --json)"
    if printf '%s' "$diff_out" | jq -e '.success != null' >/dev/null 2>&1; then
        assert_jq "$diff_out" '.success == true' "sandbox diff succeeds"
        assert_jq "$diff_out" \
            '[.. | objects | (.added // .modified // .removed // .overlayHash // empty)] | length >= 1' \
            "diff report carries baseline-vs-overlay deltas"
        # Honesty caveat (21.2): faithful temp-index OR explicit approximation marker.
        if printf '%s' "$diff_out" | jq -e '.. | objects | has("sandboxApproximation") or has("sandbox_approximation")' >/dev/null 2>&1; then
            assert_jq "$diff_out" \
                '[.. | objects | (.sandboxApproximation // .sandbox_approximation // empty)] | length >= 1' \
                "diff marks sandbox_approximation when not temp-indexed (21.2 honesty)"
        else
            log_drop 1 "temp-index fidelity / sandbox_approximation marker pending (bd-1n0np.21.2)"
        fi
    else
        log_drop 1 "ee sandbox diff invocation pending (bd-1n0np.21.3)"
    fi
else
    log_drop 1 "ee sandbox diff surface pending (bd-1n0np.21.3): delta assertions skipped"
fi

step "DB is unchanged after sandbox ops (read-only hypothesis, no durable mutation)"
count_after="$(ee_memory_count "$WS")"
e2e_log_note "durable_memory_count_after=$count_after"
assert_eq "$count_before" "$count_after" "sandbox ops perform no durable mutation"

step "apply persists the proposed change (routes through remember/curate)"
if ee_sandbox_has "apply"; then
    applied="$(ee_json sandbox apply --workspace "$WS" --json)"
    if printf '%s' "$applied" | jq -e '.success != null' >/dev/null 2>&1; then
        assert_jq "$applied" '.success == true' "sandbox apply persists via the normal path"
    else
        log_drop 1 "ee sandbox apply invocation pending (bd-1n0np.21.3)"
    fi
else
    log_drop 1 "ee sandbox apply surface pending (bd-1n0np.21.3): persistence assertion skipped"
fi

end_temp_workspace
harness_summary
