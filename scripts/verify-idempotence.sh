#!/usr/bin/env bash
# scripts/verify-idempotence.sh — Phase 5 idempotence test for ee doctor.
#
# For each tested fixture: run `ee doctor --fix` twice. Both runs must emit
# an ee.response.v2 envelope with ee.doctor.fix_summary.v1 data, and the
# SECOND run must be idempotent, which means all of (bd-2oh15 strand 4,
# ruling c9954):
#   (a) the workspace bytes are identical across the second run;
#   (b) the second run takes zero NON-guidance actions
#       (actionCount - guidanceOnlyFixerCount == 0);
#   (c) the second run records the same guidance set as the first (the
#       finding codes of its guidance_recorded fixers).
# Re-issuing the same guidance is correct: the problem is still there. A
# changed or extra guidance on a repeat run is a real idempotence failure.
#
# Fixtures are bucketed by manifest label like verify-undo.sh: COVERAGE and
# GAP fixtures run; UNTESTED (UNCLASSIFIED, UNRESOLVED) marker-only fixtures
# are not run, never count as passes and never fail this script.
#
# `--fix` and `--only <FM>` conflict at the CLI level, so both invocations run
# the full doctor without the per-FM filter.
#
# Driven by scripts/run-safety-harness.sh.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURE_ROOT="${EE_DOCTOR_FIXTURE_ROOT:-${TMPDIR:-/tmp}/ee-doctor-fixtures-idem}"
FIXTURES_SRC="${EE_DOCTOR_FIXTURES_SRC:-$REPO_ROOT/tests/doctor_fixtures}"
MANIFEST="$FIXTURES_SRC/manifest.json"
EE_BIN="${EE_DOCTOR_FIXTURE_BINARY:-ee}"

if ! command -v "$EE_BIN" >/dev/null 2>&1; then
    # A harness that ran nothing is not a pass (bd-2oh15 ruling, option a).
    echo "verify-idempotence: SKIPPED: ee binary '$EE_BIN' not found; set EE_DOCTOR_FIXTURE_BINARY" >&2
    exit 3
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "verify-idempotence: jq required" >&2
    exit 64
fi
if [ ! -f "$MANIFEST" ]; then
    echo "verify-idempotence: fixture manifest $MANIFEST missing" >&2
    exit 64
fi
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$FIXTURES_SRC/lib.sh"

mkdir -p "$FIXTURE_ROOT"
PASS=0
FAIL=0
SKIP=0
UNTESTED=0
FAILED_FMS=""
SKIPPED_FMS=""

guidance_set() {
    jq -r '[.data.fixerResults[]? | select(.outcome == "guidance_recorded") | .findingCode] | unique | join(",")' "$1"
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
        coverage | gap) ;;
        *)
            FAIL=$((FAIL + 1))
            FAILED_FMS="$FAILED_FMS $fm_id(label:${bucket#unknown:})"
            echo "verify-idempotence[$fm_id]: no usable manifest label ('${bucket#unknown:}')" >&2
            continue
            ;;
    esac
    target="$FIXTURE_ROOT/$fm_id"
    mkdir -p "$target"

    # Don't silently swallow corrupt.sh failures: count as SKIP and exit
    # non-zero if any fixture was skipped.
    if ! EE_DOCTOR_FIXTURE_TARGET="$target" "$fm_dir/corrupt.sh" >/dev/null 2>&1; then
        SKIP=$((SKIP + 1))
        SKIPPED_FMS="$SKIPPED_FMS $fm_id"
        echo "verify-idempotence[$fm_id]: corrupt.sh failed; counted as SKIP" >&2
        continue
    fi

    # Every harness artifact lives under .fixture_baseline/, which the content
    # digest excludes: a digest file written INTO the digested tree changes the
    # next digest and makes every fixture look non-idempotent.
    work="$target/.fixture_baseline"
    mkdir -p "$work"
    "$EE_BIN" doctor --workspace "$target" --fix --json > "$work/idem-fix1.json" 2>/dev/null || true
    doctor_fixture_content_digest "$target" > "$work/idem-before-run2.sha256"
    "$EE_BIN" doctor --workspace "$target" --fix --json > "$work/idem-fix2.json" 2>/dev/null || true
    doctor_fixture_content_digest "$target" > "$work/idem-after-run2.sha256"

    # Both runs MUST emit a successful ee.response.v2 envelope with typed
    # ee.doctor.fix_summary.v1 data. If the CLI rejected the invocation, it
    # would emit ee.error.v2.
    fix1_contract=$(jq -r '[.schema // "", (.success // false | tostring), .data.schema // ""] | join("|")' "$work/idem-fix1.json" 2>/dev/null)
    fix2_contract=$(jq -r '[.schema // "", (.success // false | tostring), .data.schema // ""] | join("|")' "$work/idem-fix2.json" 2>/dev/null)
    expected_contract="ee.response.v2|true|ee.doctor.fix_summary.v1"
    if [ "$fix1_contract" != "$expected_contract" ] || [ "$fix2_contract" != "$expected_contract" ]; then
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id(contract_drift:$fix1_contract,$fix2_contract)"
        echo "verify-idempotence[$fm_id]: expected $expected_contract, got run1=$fix1_contract run2=$fix2_contract" >&2
        continue
    fi

    # (a) bytes identical across the second run.
    if ! cmp -s "$work/idem-before-run2.sha256" "$work/idem-after-run2.sha256"; then
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id(bytes_changed)"
        echo "verify-idempotence[$fm_id]: the second --fix changed workspace bytes" >&2
        continue
    fi
    # (b) zero non-guidance actions on the second run.
    non_guidance=$(jq -r '((.data.actionCount // 0) - (.data.guidanceOnlyFixerCount // 0)) | tostring' "$work/idem-fix2.json" 2>/dev/null || echo "")
    if [ "$non_guidance" != "0" ]; then
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id(non_guidance=$non_guidance)"
        echo "verify-idempotence[$fm_id]: expected 0 non-guidance actions on the second run, got '$non_guidance'" >&2
        continue
    fi
    # (c) the same guidance set as the first run.
    guidance1="$(guidance_set "$work/idem-fix1.json")"
    guidance2="$(guidance_set "$work/idem-fix2.json")"
    if [ "$guidance1" != "$guidance2" ]; then
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id(guidance:[$guidance1]->[$guidance2])"
        echo "verify-idempotence[$fm_id]: guidance changed on the repeat run: [$guidance1] -> [$guidance2]" >&2
        continue
    fi
    PASS=$((PASS + 1))
done
shopt -u nullglob

echo "verify-idempotence: passed=$PASS failed=$FAIL skipped=$SKIP; $UNTESTED UNTESTED (not run)" >&2
# Every verdict below is reported; none hides another behind an early exit.
status=0
if [ $((PASS + FAIL + SKIP)) -eq 0 ]; then
    echo "verify-idempotence: no COVERAGE or GAP fixture ran; a run that tests nothing is not a pass" >&2
    status=1
fi
doctor_fixture_untested_ratchet verify-idempotence "$FIXTURES_SRC" || status=1
if [ "$FAIL" -gt 0 ]; then
    echo "verify-idempotence: failed:$FAILED_FMS" >&2
    status=1
fi
if [ "$SKIP" -gt 0 ]; then
    echo "verify-idempotence: skipped:$SKIPPED_FMS (corrupt.sh broken — refusing to declare success)" >&2
    status=1
fi
exit "$status"
