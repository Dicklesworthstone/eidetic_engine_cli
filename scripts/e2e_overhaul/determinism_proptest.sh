#!/usr/bin/env bash
# N4.5 - Typed determinism proptest e2e driver.
#
# Runs the persisted-regression preflight, the 1024-case seeded pack property,
# and a small copied-store context replay property. Agents can force remote
# Cargo execution with EE_DETERMINISM_PROPTEST_USE_RCH=1.
#
# shellcheck disable=SC2329

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=scripts/lib/e2e_logger.sh
source "$REPO_ROOT/scripts/lib/e2e_logger.sh"

run_status=0
cases_sampled=0
cases_failed=0
START_SECONDS="$(python3 -c 'import time; print(time.time())')"
BUDGET_SECONDS="${EE_DETERMINISM_PROPTEST_BUDGET_SECONDS:-60}"

run_cargo_gate() {
    local label="$1"
    shift

    if [ "${EE_DETERMINISM_PROPTEST_USE_RCH:-0}" = "1" ]; then
        if "$REPO_ROOT/scripts/rch_verify.sh" \
            --bead-id bd-17c65.14.4.5 \
            --summary \
            --no-write \
            --project-root "$REPO_ROOT" \
            -- "$@"; then
            e2e_log_assert_eq "true" "true" "$label"
            return 0
        fi
    elif (cd "$REPO_ROOT" && "$@"); then
        e2e_log_assert_eq "true" "true" "$label"
        return 0
    fi

    e2e_log_assert_eq "failed" "passed" "$label"
    run_status=1
    cases_failed=$((cases_failed + 1))
    return 1
}

elapsed_seconds() {
    python3 - "$START_SECONDS" <<'PY'
import sys
import time

started = float(sys.argv[1])
print(f"{time.time() - started:.3f}")
PY
}

emit_proptest_summary() {
    local elapsed
    elapsed="$(elapsed_seconds)"
    local cases_passed=$((cases_sampled - cases_failed))
    if [ "$cases_passed" -lt 0 ]; then
        cases_passed=0
    fi
    _e2e_emit_event "proptest_run" \
        "axes_count" "6" \
        "cases_sampled" "$cases_sampled" \
        "cases_passed" "$cases_passed" \
        "cases_failed" "$cases_failed" \
        "new_regressions" "[]" \
        "stale_regressions_flagged" "[]" \
        "elapsed_seconds" "$elapsed" \
        "budget_seconds" "$BUDGET_SECONDS"
}

e2e_log_start "determinism_proptest"
trap 'emit_proptest_summary; e2e_log_end' EXIT

if [ "${EE_DETERMINISM_PROPTEST_PLAN_ONLY:-0}" = "1" ]; then
    printf 'determinism_proptest use_rch=%s budget_seconds=%s long=%s\n' \
        "${EE_DETERMINISM_PROPTEST_USE_RCH:-0}" \
        "$BUDGET_SECONDS" \
        "${EE_PROPTEST_LONG:-0}"
    exit 0
fi

run_cargo_gate \
    "determinism_proptest_regression_preflight" \
    cargo test --test property_query_and_pack \
        determinism_regression_fixtures_replay_before_sampling \
        -- --exact --nocapture || true

cases_sampled=$((cases_sampled + 1024))
run_cargo_gate \
    "determinism_proptest_seeded_pack_1024_cases" \
    cargo test --test property_query_and_pack \
        seeded_pack_assembly_replays_byte_identical_output \
        -- --exact --nocapture || true

cases_sampled=$((cases_sampled + 16))
run_cargo_gate \
    "determinism_proptest_context_copied_store_16_cases" \
    cargo test --test property_query_and_pack \
        context_pack_json_replays_across_copied_store_tuple \
        -- --exact --nocapture || true

if [ "${EE_PROPTEST_LONG:-0}" = "1" ]; then
    run_cargo_gate \
        "determinism_proptest_full_property_query_and_pack" \
        cargo test --test property_query_and_pack -- --nocapture || true
fi

exit "$run_status"
