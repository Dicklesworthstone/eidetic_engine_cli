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

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EE_BIN="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
FIXTURE_ROOT="${EE_DOCTOR_FIXTURE_ROOT:-${TMPDIR:-/tmp}/ee-doctor-fixtures-metamorphic}"
FIXTURES_SRC="${EE_DOCTOR_FIXTURES_SRC:-$REPO_ROOT/tests/doctor_fixtures}"

if ! command -v "$EE_BIN" >/dev/null 2>&1; then
    echo "verify-metamorphic: ee binary '$EE_BIN' not on PATH; skipping (advisory)" >&2
    exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "verify-metamorphic: jq required" >&2
    exit 64
fi

mkdir -p "$FIXTURE_ROOT"
PASS=0
FAIL=0
SKIP=0
FAILED_FMS=""
SKIPPED_FMS=""

shopt -s nullglob
for fm_dir in "$FIXTURES_SRC"/fm-*; do
    fm_id="$(basename "$fm_dir")"
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
    "$EE_BIN" doctor --workspace "$target" --json > "$target/.diag1.json" 2>/dev/null || true
    "$EE_BIN" doctor --workspace "$target" --json > "$target/.diag2.json" 2>/dev/null || true

    h1=$(jq 'walk(if type == "object" then with_entries(select(.key | test("^(committed_at|started_at|finished_at|ts|now|generatedAt|durationMs|elapsedMs)$") | not)) else . end)' "$target/.diag1.json" 2>/dev/null | shasum -a 256 | awk '{print $1}')
    h2=$(jq 'walk(if type == "object" then with_entries(select(.key | test("^(committed_at|started_at|finished_at|ts|now|generatedAt|durationMs|elapsedMs)$") | not)) else . end)' "$target/.diag2.json" 2>/dev/null | shasum -a 256 | awk '{print $1}')

    if [ -n "$h1" ] && [ "$h1" = "$h2" ]; then
        PASS=$((PASS + 1))
    else
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id"
        echo "verify-metamorphic[$fm_id]: diagnose output non-deterministic" >&2
        diff <(jq -S . "$target/.diag1.json") <(jq -S . "$target/.diag2.json") 2>&1 | head -30 >&2 || true
    fi
done
shopt -u nullglob

echo "verify-metamorphic: passed=$PASS failed=$FAIL skipped=$SKIP" >&2
if [ "$FAIL" -gt 0 ]; then
    echo "verify-metamorphic: failed:$FAILED_FMS" >&2
    exit 1
fi
if [ "$SKIP" -gt 0 ]; then
    echo "verify-metamorphic: skipped:$SKIPPED_FMS (corrupt.sh broken — refusing to declare success)" >&2
    exit 1
fi
exit 0
