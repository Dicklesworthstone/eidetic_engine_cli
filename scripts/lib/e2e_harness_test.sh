#!/bin/bash
# Tests for scripts/lib/e2e_harness.sh. Currently: the vacuity contract.
#
# WHY THIS EXISTS. harness_summary computed `verdict = PASS if fail == 0`, with
# no reference to how many assertions actually succeeded. So a script that
# dropped every arm for a missing prerequisite emitted verdict PASS and exited
# 0, having verified nothing. Demonstrated directly before the change:
#
#     [harness] vacuity_demo: 0 pass, 0 fail, 0 steps, 1 drops, 49.351ms -> PASS
#     {'pass': 0, 'fail': 0, 'drops': 1, 'verdict': 'PASS'}   rc=0
#
# That is CORRECT for an ad-hoc run -- a host that genuinely cannot satisfy a
# script should not be forced to fake an answer, and several scripts here
# deliberately refuse to substitute an emulator -- and it is exactly the
# constant-green shape a verify.sh stage must never have
# (bd-unreachable-e2e-scripts-u14sr quantifies the exposure: 22 of 207 e2e
# scripts call log_drop, 16 of them already wired).
#
# So the FACT is always reported and the REFUSAL is opt-in:
#   * `asserted` is always in summary.json, and the human line says VACUOUS;
#   * EE_E2E_REFUSE_VACUOUS_PASS=1 turns it into verdict VACUOUS and rc 2.
#
# THE FIRST THREE ARMS ARE THE IMPORTANT ONES. They pin that the default is
# byte-for-byte what it was for all 207 callers; a change to a shared harness
# that alters any existing verdict is a change to every gate at once.
#
# harness_init is deliberately NOT used: it calls ee_require_current_binary,
# which correctly refuses on a host with no current native binary, and the
# summary contract does not depend on a binary. Setting the counters directly
# is what makes this runnable with no toolchain and no store.
set -u

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT" || exit 70

WORK="${TMPDIR:-/tmp}/ee_harness_test.$$"
mkdir -p "$WORK" || exit 70
FAILURES=0

run_case() {
    local name="$1" refuse="$2" p="$3" f="$4" drops="$5"
    local dir="$WORK/$name"
    mkdir -p "$dir"
    (
        source scripts/lib/e2e_harness.sh >/dev/null 2>&1
        HARNESS_TEST_NAME="$name"
        LOG_DIR="$dir"
        EE_TEST_LOG_PATH="$dir/events.jsonl"
        : > "$EE_TEST_LOG_PATH"
        HARNESS_START_NS="$(_harness_now_ns)"
        HARNESS_PASS="$p"
        HARNESS_FAIL="$f"
        HARNESS_STEP=0
        HARNESS_DROPS="$drops"
        HARNESS_FAILURES=()
        if [ "$f" -gt 0 ]; then HARNESS_FAILURES=("synthetic failure"); fi
        export EE_E2E_REFUSE_VACUOUS_PASS="$refuse"
        harness_summary >"$dir/out.txt" 2>"$dir/err.txt"
        echo "$?" >"$dir/rc"
    )
}

expect() {
    local name="$1" want="$2" refuse="$3" p="$4" f="$5" drops="$6"
    run_case "$name" "$refuse" "$p" "$f" "$drops"
    local dir="$WORK/$name" rc got
    rc="$(cat "$dir/rc")"
    if [ ! -f "$dir/summary.json" ]; then
        printf '  FAIL  %-30s no summary.json was written\n' "$name"
        FAILURES=$((FAILURES + 1))
        return
    fi
    got="$rc|$(python3 -c "
import json
d = json.load(open('$dir/summary.json'))
print(f\"{d['verdict']}|{d['asserted']}\")
")"
    if [ "$got" = "$want" ]; then
        printf '  ok    %-30s %s\n' "$name" "$got"
    else
        printf '  FAIL  %-30s want=%s got=%s\n' "$name" "$want" "$got"
        FAILURES=$((FAILURES + 1))
    fi
}

printf 'e2e_harness vacuity contract\n'

# --- the default must not move, for any caller ------------------------------
expect default_vacuous  "0|PASS|False" 0 0 0 3
expect default_passing  "0|PASS|True"  0 7 0 0
expect default_failing  "1|FAIL|True"  0 3 2 0

# --- opt-in refusal ---------------------------------------------------------
expect refuse_vacuous   "2|VACUOUS|False" 1 0 0 3
expect refuse_passing   "0|PASS|True"     1 7 0 0

# A real failure OUTRANKS vacuity: rc 1, so a caller that special-cases 2 as
# "answered nothing" never swallows a genuine red.
expect refuse_failing   "1|FAIL|True"     1 3 2 0

# Vacuity is "asserted nothing", NOT "dropped something". A run with no drops
# and no assertions is still vacuous; keying on HARNESS_DROPS would miss it.
expect refuse_no_drops  "2|VACUOUS|False" 1 0 0 0

# An unset flag must behave as 0 rather than tripping `set -u` in the caller.
(
    source scripts/lib/e2e_harness.sh >/dev/null 2>&1
    dir="$WORK/unset_flag"
    mkdir -p "$dir"
    HARNESS_TEST_NAME="unset_flag"
    LOG_DIR="$dir"
    EE_TEST_LOG_PATH="$dir/events.jsonl"
    : > "$EE_TEST_LOG_PATH"
    HARNESS_START_NS="$(_harness_now_ns)"
    HARNESS_PASS=0; HARNESS_FAIL=0; HARNESS_STEP=0; HARNESS_DROPS=1
    HARNESS_FAILURES=()
    unset EE_E2E_REFUSE_VACUOUS_PASS
    harness_summary >"$dir/out.txt" 2>"$dir/err.txt"
    echo "$?" >"$dir/rc"
)
if [ "$(cat "$WORK/unset_flag/rc")" = "0" ]; then
    printf '  ok    %-30s 0 (unset flag defaults to permissive)\n' "unset_flag"
else
    printf '  FAIL  %-30s rc=%s, expected 0\n' "unset_flag" "$(cat "$WORK/unset_flag/rc")"
    FAILURES=$((FAILURES + 1))
fi

# The human line must name vacuity even when refusal is OFF, or a reader has to
# know which flag was set to learn that nothing asserted.
if grep -q 'VACUOUS' "$WORK/default_vacuous/err.txt"; then
    printf '  ok    %-30s human line names vacuity with refusal off\n' "default_visibility"
else
    printf '  FAIL  %-30s human line hid vacuity: %s\n' "default_visibility" \
        "$(grep '\[harness\]' "$WORK/default_vacuous/err.txt" | head -1)"
    FAILURES=$((FAILURES + 1))
fi

printf '\n'
if [ "$FAILURES" -ne 0 ]; then
    printf 'e2e_harness_test: %d arm(s) FAILED (work kept at %s)\n' "$FAILURES" "$WORK"
    exit 1
fi
printf 'e2e_harness_test: 9/9 arms passed\n'
exit 0
