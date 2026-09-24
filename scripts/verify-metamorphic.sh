#!/usr/bin/env bash
# scripts/verify-metamorphic.sh — Phase 5 detector-repeatability test.
#
# Asserts that the doctor's pure-detector path returns the IDENTICAL
# finding set across two read-only invocations against the same
# unchanged workspace. The metamorphic relation:
#
#   detect(state) == detect(state)
#
# Run `ee doctor --workspace <fixture> --json` twice with no mutations
# between, diff the report.checks[] arrays. Any difference is a bug
# (non-deterministic detector).
#
# Fixtures are bucketed by manifest label like verify-undo.sh (bd-2oh15
# rulings): COVERAGE and GAP fixtures run; UNTESTED (UNCLASSIFIED,
# UNRESOLVED) marker-only fixtures are not run and never count as passes,
# held to the shared pin (doctor_fixture_untested_ratchet).
#
# A fixture whose damage is not in the store's bytes (an environment, a live
# lock holder) ships a condition.sh, and both runs go through
# doctor_fixture_under_condition (bd-2oh15 ruling t2250 R2). A run whose
# condition could not be applied is counted as condition_not_applied: never a
# pass, and it fails this script.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EE_BIN="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
FIXTURE_ROOT="${EE_DOCTOR_FIXTURE_ROOT:-${TMPDIR:-/tmp}/ee-doctor-fixtures-metamorphic}"
FIXTURES_SRC="${EE_DOCTOR_FIXTURES_SRC:-$REPO_ROOT/tests/doctor_fixtures}"
MANIFEST="$FIXTURES_SRC/manifest.json"

if ! command -v "$EE_BIN" >/dev/null 2>&1; then
    # A harness that ran nothing is not a pass (bd-2oh15 ruling, option a).
    echo "verify-metamorphic: SKIPPED: ee binary '$EE_BIN' not found; set EE_DOCTOR_FIXTURE_BINARY" >&2
    exit 3
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "verify-metamorphic: jq required" >&2
    exit 64
fi
if [ ! -f "$MANIFEST" ]; then
    echo "verify-metamorphic: fixture manifest $MANIFEST missing" >&2
    exit 64
fi
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$FIXTURES_SRC/lib.sh"

mkdir -p "$FIXTURE_ROOT"
PASS=0
FAIL=0
SKIP=0
UNTESTED=0
NOT_APPLIED=0
CONDITIONED=0
FAILED_FMS=""
SKIPPED_FMS=""
NOT_APPLIED_FMS=""

normalize_doctor_json() {
    local input="$1"
    local output="$2"
    jq 'walk(if type == "object" then with_entries(select(.key | test("^(committed_at|started_at|finished_at|ts|now|generatedAt|durationMs|elapsedMs)$") | not)) else . end)' "$input" >"$output"
}

hash_file() {
    local input="$1"
    shasum -a 256 "$input" | awk '{print $1}'
}

shopt -s nullglob
for fm_dir in "$FIXTURES_SRC"/fm-*; do
    fm_id="$(basename "$fm_dir")"
    bucket="$(doctor_fixture_bucket "$fm_id" "$MANIFEST")"
    case "$bucket" in
        untested)
            UNTESTED=$((UNTESTED + 1))
            continue
            ;;
        out_of_scope) continue ;; # never run; listed by the ratchet line

        coverage | gap) ;;
        *)
            FAIL=$((FAIL + 1))
            FAILED_FMS="$FAILED_FMS $fm_id(label:${bucket#unknown:})"
            echo "verify-metamorphic[$fm_id]: no usable manifest label ('${bucket#unknown:}')" >&2
            continue
            ;;
    esac
    target="$FIXTURE_ROOT/$fm_id"
    mkdir -p "$target"
    # Round-6 self-review: don't silently swallow corrupt.sh failures —
    # count as SKIP and exit non-zero if any fixture was skipped, so
    # missing-fixture regressions can't masquerade as PASS.
    if ! EE_DOCTOR_FIXTURE_TARGET="$target" "$fm_dir/corrupt.sh" >/dev/null 2>&1; then
        SKIP=$((SKIP + 1))
        SKIPPED_FMS="$SKIPPED_FMS $fm_id"
        echo "verify-metamorphic[$fm_id]: corrupt.sh failed; counted as SKIP" >&2
        continue
    fi

    # Two read-only diagnose runs. Normalize timestamps (ee.doctor.action.v1
    # would have timestamps if present; the read-only doctor envelope may
    # too) by stripping any keys named `committed_at`, `started_at`,
    # `finished_at`, `ts`, `now`.
    rc1=0
    doctor_fixture_under_condition "$fm_dir" "$target" \
        "$EE_BIN" doctor --workspace "$target" --json > "$target/.diag1.json" 2> "$target/.diag1.err" || rc1=$?
    rc2=0
    doctor_fixture_under_condition "$fm_dir" "$target" \
        "$EE_BIN" doctor --workspace "$target" --json > "$target/.diag2.json" 2> "$target/.diag2.err" || rc2=$?
    if [ "$rc1" -eq "$DOCTOR_FIXTURE_CONDITION_NOT_APPLIED" ] || [ "$rc2" -eq "$DOCTOR_FIXTURE_CONDITION_NOT_APPLIED" ]; then
        NOT_APPLIED=$((NOT_APPLIED + 1))
        NOT_APPLIED_FMS="$NOT_APPLIED_FMS $fm_id"
        echo "verify-metamorphic[$fm_id]: condition not applied; not a pass: $(cat "$target/.diag1.err" "$target/.diag2.err" | grep -m1 'condition:' || true)" >&2
        continue
    fi

    if ! normalize_doctor_json "$target/.diag1.json" "$target/.diag1.normalized.json" 2>/dev/null; then
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id"
        echo "verify-metamorphic[$fm_id]: first diagnose output was not valid JSON" >&2
        continue
    fi
    if ! normalize_doctor_json "$target/.diag2.json" "$target/.diag2.normalized.json" 2>/dev/null; then
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id"
        echo "verify-metamorphic[$fm_id]: second diagnose output was not valid JSON" >&2
        continue
    fi

    h1=$(hash_file "$target/.diag1.normalized.json")
    h2=$(hash_file "$target/.diag2.normalized.json")

    if [ -n "$h1" ] && [ "$h1" = "$h2" ]; then
        PASS=$((PASS + 1))
        if [ -f "$fm_dir/condition.sh" ]; then
            CONDITIONED=$((CONDITIONED + 1))
        fi
    else
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id"
        echo "verify-metamorphic[$fm_id]: diagnose output non-deterministic" >&2
        diff <(jq -S . "$target/.diag1.normalized.json") <(jq -S . "$target/.diag2.normalized.json") 2>&1 | head -30 >&2 || true
    fi
done
shopt -u nullglob

echo "verify-metamorphic: passed=$PASS failed=$FAIL skipped=$SKIP condition_not_applied=$NOT_APPLIED; $UNTESTED UNTESTED (not run); $CONDITIONED of the passes ran under their fixture's condition" >&2
# Every verdict below is reported; none hides another behind an early exit.
status=0
if [ $((PASS + FAIL + SKIP + NOT_APPLIED)) -eq 0 ]; then
    echo "verify-metamorphic: no COVERAGE or GAP fixture ran; a run that tests nothing is not a pass" >&2
    status=1
fi
doctor_fixture_untested_ratchet verify-metamorphic "$FIXTURES_SRC" || status=1
if [ "$FAIL" -gt 0 ]; then
    echo "verify-metamorphic: failed:$FAILED_FMS" >&2
    status=1
fi
if [ "$SKIP" -gt 0 ]; then
    echo "verify-metamorphic: skipped:$SKIPPED_FMS (corrupt.sh broken — refusing to declare success)" >&2
    status=1
fi
if [ "$NOT_APPLIED" -gt 0 ]; then
    echo "verify-metamorphic: condition_not_applied:$NOT_APPLIED_FMS (the damage was not in place — refusing to declare success)" >&2
    status=1
fi
exit "$status"
