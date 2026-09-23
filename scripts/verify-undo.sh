#!/usr/bin/env bash
# scripts/verify-undo.sh — Phase 5 reversibility test for ee doctor.
#
# Per fixture under tests/doctor_fixtures/fm-*/, runs corrupt.sh and then
# assert.sh with EE_DOCTOR_FIXTURE_RUN_EE=1. Each fixture's assert.sh owns its
# oracle: a repair round trip (fix -> undo -> byte-identical to the corrupted
# baseline), guidance only, or a pinned gap.
#
# Fixtures are counted in three buckets by their manifest label (bd-2oh15
# strand 4, ruling c9954):
#   COVERAGE   REPAIR, GUIDANCE-ONLY. Must pass; a failure fails the run.
#   GAP        NOT-DETECTED, PINNED-DEFECT. Must still reproduce the pinned
#              gap; if a gap stops reproducing the label is wrong, and that
#              fails the run too.
#   UNTESTED   UNCLASSIFIED, UNRESOLVED (marker-only). Not run. They never
#              count as passes and never fail the run, but their number may
#              only go down: the pins below fail the run if it grows.
#
# Driven by scripts/run-safety-harness.sh (bd-21joy stage 8.5 of
# scripts/verify.sh). Exits 0 when every COVERAGE fixture passes, every GAP
# still reproduces and the UNTESTED pins hold; 1 otherwise; 64 on usage error.
# Honors EE_DOCTOR_FIXTURE_ROOT, EE_DOCTOR_FIXTURES_SRC and
# EE_DOCTOR_FIXTURE_BINARY.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURE_ROOT="${EE_DOCTOR_FIXTURE_ROOT:-${TMPDIR:-/tmp}/ee-doctor-fixtures}"
FIXTURES_SRC="${EE_DOCTOR_FIXTURES_SRC:-$REPO_ROOT/tests/doctor_fixtures}"
MANIFEST="$FIXTURES_SRC/manifest.json"
EE_BIN="${EE_DOCTOR_FIXTURE_BINARY:-ee}"

# Ratchet pins (ruling c9954): lower these when a fixture is classified; never
# raise them. A new fixture must arrive classified.
MAX_UNCLASSIFIED=14
MAX_UNRESOLVED=1

if ! command -v "$EE_BIN" >/dev/null 2>&1; then
    # A harness that ran nothing is not a pass (bd-2oh15 ruling, option a).
    echo "verify-undo: SKIPPED: ee binary '$EE_BIN' not found; set EE_DOCTOR_FIXTURE_BINARY" >&2
    exit 3
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "verify-undo: jq required" >&2
    exit 64
fi
if [ ! -f "$MANIFEST" ]; then
    echo "verify-undo: fixture manifest $MANIFEST missing" >&2
    exit 64
fi
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$FIXTURES_SRC/lib.sh"

mkdir -p "$FIXTURE_ROOT"
COVERAGE_PASS=0
COVERAGE_FAIL=0
GAP_REPRODUCED=0
GAP_LOST=0
UNCLASSIFIED=0
UNRESOLVED=0
SKIP=0
UNKNOWN=0
FAILED_FMS=""

shopt -s nullglob
for fm_dir in "$FIXTURES_SRC"/fm-*; do
    fm_id="$(basename "$fm_dir")"
    bucket="$(doctor_fixture_bucket "$fm_id" "$MANIFEST")"
    case "$bucket" in
        untested)
            if [ "$(doctor_fixture_label "$fm_id" "$MANIFEST")" = "UNRESOLVED" ]; then
                UNRESOLVED=$((UNRESOLVED + 1))
            else
                UNCLASSIFIED=$((UNCLASSIFIED + 1))
            fi
            continue
            ;;
        coverage | gap) ;;
        *)
            UNKNOWN=$((UNKNOWN + 1))
            FAILED_FMS="$FAILED_FMS $fm_id(label:${bucket#unknown:})"
            echo "verify-undo[$fm_id]: no usable manifest label ('${bucket#unknown:}')" >&2
            continue
            ;;
    esac

    target="$FIXTURE_ROOT/$fm_id"
    mkdir -p "$target"
    # A corrupt.sh failure is a SKIP, and any SKIP fails the run: a missing
    # fixture must not look like a pass.
    if ! EE_DOCTOR_FIXTURE_TARGET="$target" "$fm_dir/corrupt.sh" >/dev/null 2>&1; then
        SKIP=$((SKIP + 1))
        FAILED_FMS="$FAILED_FMS $fm_id(corrupt.sh)"
        echo "verify-undo[$fm_id]: corrupt.sh failed; counted as SKIP" >&2
        continue
    fi

    if EE_DOCTOR_FIXTURE_TARGET="$target" \
       EE_DOCTOR_FIXTURE_RUN_EE=1 \
       EE_DOCTOR_FIXTURE_BINARY="$EE_BIN" \
       "$fm_dir/assert.sh" >"$target/.assert.stdout" 2>"$target/.assert.stderr"; then
        if [ "$bucket" = coverage ]; then
            COVERAGE_PASS=$((COVERAGE_PASS + 1))
        else
            GAP_REPRODUCED=$((GAP_REPRODUCED + 1))
        fi
    else
        if [ "$bucket" = coverage ]; then
            COVERAGE_FAIL=$((COVERAGE_FAIL + 1))
            FAILED_FMS="$FAILED_FMS $fm_id(coverage)"
            echo "verify-undo[$fm_id]: COVERAGE assert FAILED" >&2
        else
            GAP_LOST=$((GAP_LOST + 1))
            FAILED_FMS="$FAILED_FMS $fm_id(gap)"
            echo "verify-undo[$fm_id]: GAP no longer reproduces; the label is wrong, relabel it" >&2
        fi
        tail -20 "$target/.assert.stderr" >&2 || true
    fi
done
shopt -u nullglob

echo "verify-undo: COVERAGE passed=$COVERAGE_PASS failed=$COVERAGE_FAIL; GAP reproduced=$GAP_REPRODUCED lost=$GAP_LOST; $UNCLASSIFIED UNCLASSIFIED (not tested, pin $MAX_UNCLASSIFIED); $UNRESOLVED UNRESOLVED (not tested, pin $MAX_UNRESOLVED); skipped=$SKIP unknown=$UNKNOWN fixture_root=$FIXTURE_ROOT" >&2

status=0
if [ $((COVERAGE_PASS + COVERAGE_FAIL + GAP_REPRODUCED + GAP_LOST)) -eq 0 ]; then
    echo "verify-undo: no COVERAGE or GAP fixture ran; a run that tests nothing is not a pass" >&2
    status=1
fi
if [ "$UNCLASSIFIED" -gt "$MAX_UNCLASSIFIED" ] || [ "$UNRESOLVED" -gt "$MAX_UNRESOLVED" ]; then
    echo "verify-undo: UNTESTED ratchet exceeded (UNCLASSIFIED $UNCLASSIFIED > $MAX_UNCLASSIFIED or UNRESOLVED $UNRESOLVED > $MAX_UNRESOLVED); classify the new fixture" >&2
    status=1
fi
if [ -n "$FAILED_FMS" ]; then
    echo "verify-undo: failed FMs:$FAILED_FMS" >&2
    status=1
fi
exit "$status"
