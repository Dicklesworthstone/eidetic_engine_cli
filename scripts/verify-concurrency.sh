#!/usr/bin/env bash
# scripts/verify-concurrency.sh — Phase 5 concurrency-safety test.
#
# For every COVERAGE fixture (manifest label REPAIR or GUIDANCE-ONLY: a real
# damaged store that --fix would act on), builds the fixture, holds the exact
# persistent doctor lock with the platform `flock` primitive, then launches
# `ee doctor --fix` against the same workspace. Asserts the CLI refuses with
# its typed concurrency error (doctor_concurrency_lost, phase start, nonzero
# exit) and that the refused run changed no workspace byte.
#
# Vacuity guard (bd-2oh15 strand 4, ruling c9954): the number of fixtures
# exercised is announced, and zero fails the run -- including when flock is
# unavailable, since then nothing was exercised.
#
# Driven by scripts/run-safety-harness.sh.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURE_ROOT="${EE_DOCTOR_FIXTURE_ROOT:-${TMPDIR:-/tmp}/ee-doctor-fixtures-conc}"
FIXTURES_SRC="${EE_DOCTOR_FIXTURES_SRC:-$REPO_ROOT/tests/doctor_fixtures}"
MANIFEST="$FIXTURES_SRC/manifest.json"
EE_BIN="${EE_DOCTOR_FIXTURE_BINARY:-ee}"

if ! command -v "$EE_BIN" >/dev/null 2>&1; then
    # A harness that ran nothing is not a pass (bd-2oh15 ruling, option a).
    echo "verify-concurrency: SKIPPED: ee binary '$EE_BIN' not found; set EE_DOCTOR_FIXTURE_BINARY" >&2
    exit 3
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "verify-concurrency: jq required" >&2
    exit 64
fi
if ! command -v flock >/dev/null 2>&1; then
    echo "verify-concurrency: flock not available; exercised 0 fixtures -- FAIL (a run that tests nothing is not a pass)" >&2
    exit 1
fi
if [ ! -f "$MANIFEST" ]; then
    echo "verify-concurrency: fixture manifest $MANIFEST missing" >&2
    exit 64
fi
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$FIXTURES_SRC/lib.sh"
mkdir -p "$FIXTURE_ROOT"

holder_pid=""
# shellcheck disable=SC2329  # invoked indirectly by the EXIT trap and per fixture
stop_holder() {
    if [ -n "$holder_pid" ] && kill -0 "$holder_pid" 2>/dev/null; then
        kill "$holder_pid" 2>/dev/null || true
        wait "$holder_pid" 2>/dev/null || true
    fi
    holder_pid=""
}
trap stop_holder EXIT

EXERCISED=0
FAIL=0
FAILED_FMS=""

shopt -s nullglob
for fm_dir in "$FIXTURES_SRC"/fm-*; do
    fm_id="$(basename "$fm_dir")"
    [ "$(doctor_fixture_bucket "$fm_id" "$MANIFEST")" = coverage ] || continue
    target="$FIXTURE_ROOT/$fm_id"
    mkdir -p "$target"
    if ! EE_DOCTOR_FIXTURE_TARGET="$target" EE_DOCTOR_FIXTURE_BINARY="$EE_BIN" \
        "$fm_dir/corrupt.sh" >/dev/null 2>&1; then
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id(corrupt.sh)"
        echo "verify-concurrency[$fm_id]: corrupt.sh failed" >&2
        continue
    fi

    # A normal --fix finishes too quickly for a process race to be
    # deterministic, so hold the same kernel advisory lock RunContext uses.
    # Harness artifacts live under .fixture_baseline/, which the content digest
    # excludes; anything written into the digested tree would look like a
    # byte change made by the refused --fix.
    work="$target/.fixture_baseline"
    mkdir -p "$work"
    lock_path="$target/.ee/.doctor.lock"
    ready_path="$work/conc-holder-ready"
    (
        exec 9<>"$lock_path"
        flock -n 9
        printf 'verify-concurrency-holder\n%d\n' "$$" >&9
        : > "$ready_path"
        sleep 30
    ) &
    holder_pid=$!
    for _attempt in {1..100}; do
        [ -f "$ready_path" ] && break
        kill -0 "$holder_pid" 2>/dev/null || break
        sleep 0.05
    done
    if [ ! -f "$ready_path" ]; then
        stop_holder
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id(holder)"
        echo "verify-concurrency[$fm_id]: the external holder never acquired the lock" >&2
        continue
    fi

    doctor_fixture_content_digest "$target" > "$work/conc-before.sha256"
    set +e
    "$EE_BIN" doctor --workspace "$target" --fix --json > "$work/conc-run.json" 2>&1
    rc=$?
    set -e
    doctor_fixture_content_digest "$target" > "$work/conc-after.sha256"
    stop_holder
    EXERCISED=$((EXERCISED + 1))

    # The machine contract is authoritative; the nonzero exit is independently
    # required so shell callers cannot mistake the refusal for success.
    schema=$(jq -r '.schema // ""' "$work/conc-run.json" 2>/dev/null || echo "")
    code=$(jq -r '.error.code // ""' "$work/conc-run.json" 2>/dev/null || echo "")
    phase=$(jq -r '.error.details.phase // ""' "$work/conc-run.json" 2>/dev/null || echo "")
    if [ "$rc" -eq 0 ] || [ "$schema" != "ee.error.v2" ] ||
       [ "$code" != "doctor_concurrency_lost" ] || [ "$phase" != "start" ]; then
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id(refusal)"
        echo "verify-concurrency[$fm_id]: expected a nonzero typed lock refusal; rc=$rc schema=$schema code=$code phase=$phase" >&2
        continue
    fi
    if ! cmp -s "$work/conc-before.sha256" "$work/conc-after.sha256"; then
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id(bytes_changed)"
        echo "verify-concurrency[$fm_id]: the refused --fix changed workspace bytes" >&2
        continue
    fi
done
shopt -u nullglob

echo "verify-concurrency: exercised $EXERCISED COVERAGE fixtures; failed=$FAIL" >&2
if [ "$EXERCISED" -eq 0 ]; then
    echo "verify-concurrency: exercised 0 fixtures -- FAIL (a run that tests nothing is not a pass)" >&2
    exit 1
fi
if [ "$FAIL" -gt 0 ]; then
    echo "verify-concurrency: failed:$FAILED_FMS" >&2
    exit 1
fi
echo "verify-concurrency: PASS (typed lock refusal on $EXERCISED fixtures, bytes unchanged)" >&2
exit 0
