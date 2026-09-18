#!/usr/bin/env bash
# N4.5 - Typed determinism proptest e2e driver.
#
# Runs the persisted-regression preflight, the 1024-case seeded pack property,
# and a small copied-store context replay property only when explicitly routed
# through RCH. Default execution logs skipped gates and never falls back to
# local Cargo.
#
# shellcheck disable=SC2329

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=scripts/lib/e2e_logger.sh
source "$REPO_ROOT/scripts/lib/e2e_logger.sh"

run_status=0
cases_sampled=0
gates_executed=0
gates_skipped=0
cases_failed=0
START_SECONDS="$(python3 -c 'import time; print(time.time())')"
BUDGET_SECONDS="${EE_DETERMINISM_PROPTEST_BUDGET_SECONDS:-60}"

validate_nonnegative_seconds() {
    local label="$1"
    local value="$2"
    python3 - "$label" "$value" <<'PY'
import math
import sys

label = sys.argv[1]
raw = sys.argv[2]
try:
    value = float(raw)
except ValueError:
    print(f"{label} must be a finite non-negative number, got {raw!r}", file=sys.stderr)
    sys.exit(1)
if not math.isfinite(value) or value < 0:
    print(f"{label} must be a finite non-negative number, got {raw!r}", file=sys.stderr)
    sys.exit(1)
PY
}

validate_nonnegative_seconds "EE_DETERMINISM_PROPTEST_BUDGET_SECONDS" "$BUDGET_SECONDS"

run_cargo_gate() {
    local label="$1"
    local sample_count="$2"
    shift 2

    if [ "${EE_DETERMINISM_PROPTEST_USE_RCH:-0}" != "1" ]; then
        # bd-r8p6j: this used to be
        #     e2e_log_assert_eq "skipped" "skipped" "$label.remote_required"
        # which compares a constant with itself, so it ALWAYS incremented the
        # pass counter and emitted assert_ok. At the default environment every
        # gate in this script "passed", nothing executed, and the script exited
        # 0 in under a second. A skip recorded as a pass is worse than a silent
        # skip: an orphaned script announces itself in the invocation audit, a
        # permanently green stage does not.
        #
        # A skip is now recorded as a skip, in the only vocabulary this harness
        # has -- a counter this script owns. It asserts NOTHING, because a gate
        # that did not run is not evidence about anything. The truth is carried
        # by the exit status instead, below.
        gates_skipped=$((gates_skipped + 1))
        return 0
    fi

    gates_executed=$((gates_executed + 1))

    cases_sampled=$((cases_sampled + sample_count))

    if RCH_REQUIRE_REMOTE=1 "$REPO_ROOT/scripts/rch_verify.sh" \
        --bead-id bd-17c65.14.4.5 \
        --summary \
        --no-write \
        --project-root "$REPO_ROOT" \
        -- "$@"; then
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
    0 \
    cargo test --test integration_n_r \
        property_query_and_pack::determinism_regression_fixtures_replay_before_sampling \
        -- --exact --nocapture || true

run_cargo_gate \
    "determinism_proptest_seeded_pack_1024_cases" \
    1024 \
    cargo test --test integration_n_r \
        property_query_and_pack::seeded_pack_assembly_replays_byte_identical_output \
        -- --exact --nocapture || true

run_cargo_gate \
    "determinism_proptest_context_copied_store_16_cases" \
    16 \
    cargo test --test integration_n_r \
        property_query_and_pack::context_pack_json_replays_across_copied_store_tuple \
        -- --exact --nocapture || true

if [ "${EE_PROPTEST_LONG:-0}" = "1" ]; then
    run_cargo_gate \
        "determinism_proptest_full_property_query_and_pack" \
        0 \
        cargo test --test integration_n_r property_query_and_pack:: -- --nocapture || true
fi

# bd-r8p6j: NON-VACUITY GUARD. A proptest run that sampled zero cases has
# proved nothing, and this script has always known it -- emit_proptest_summary
# reports `cases_sampled: 0` in that case and nothing ever read the number.
#
# Refusing here is what stops this script being wired into verify.sh as a
# permanently green stage. It also fails LOUDLY in the default environment
# rather than passing quietly, which is the behaviour a reader can act on: the
# message names the variable that would make it run.
if [ "$gates_executed" -eq 0 ]; then
    printf 'determinism_proptest: REFUSING TO PASS. %d cargo gate(s) skipped, 0 executed, %d cases sampled.\n' \
        "$gates_skipped" "$cases_sampled" >&2
    printf '  Every gate here requires the remote lane. Set EE_DETERMINISM_PROPTEST_USE_RCH=1 to run them.\n' >&2
    printf '  Exiting non-zero on purpose: a run that executed nothing must not be reported as a pass (bd-r8p6j).\n' >&2
    exit 2
fi

exit "$run_status"
