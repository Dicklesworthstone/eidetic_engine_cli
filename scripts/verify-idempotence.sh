#!/usr/bin/env bash
# scripts/verify-idempotence.sh — Phase 5 idempotence test for ee doctor.
#
# For each fixture: run `ee doctor --fix` twice. The first run produces
# actionCount > 0 once real fixers are wired (bd-tu4s8). The second run
# reports actionCount == 0 — re-running against an already-healthy
# workspace must be a no-op.
#
# `--fix` and `--only <FM>` conflict at the CLI level (DoctorArgs marks
# them mutually exclusive). For now both invocations run the full doctor
# without the per-FM filter; the read-only diagnose pass underneath
# scripts/verify-undo.sh still uses `--only $fm_id` to confirm per-FM
# behavior, which is its own concern.
#
# Driven by scripts/run-safety-harness.sh.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURE_ROOT="${EE_DOCTOR_FIXTURE_ROOT:-${TMPDIR:-/tmp}/ee-doctor-fixtures-idem}"
FIXTURES_SRC="${EE_DOCTOR_FIXTURES_SRC:-$REPO_ROOT/tests/doctor_fixtures}"
EE_BIN="${EE_DOCTOR_FIXTURE_BINARY:-ee}"

if ! command -v "$EE_BIN" >/dev/null 2>&1; then
    echo "verify-idempotence: ee binary '$EE_BIN' not on PATH; skipping (advisory)" >&2
    exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "verify-idempotence: jq required" >&2
    exit 64
fi

mkdir -p "$FIXTURE_ROOT"
PASS=0
FAIL=0
FAILED_FMS=""

shopt -s nullglob
for fm_dir in "$FIXTURES_SRC"/fm-*; do
    fm_id="$(basename "$fm_dir")"
    target="$FIXTURE_ROOT/$fm_id"
    mkdir -p "$target"

    EE_DOCTOR_FIXTURE_TARGET="$target" "$fm_dir/corrupt.sh" >/dev/null 2>&1 || continue

    # First run.
    "$EE_BIN" doctor --workspace "$target" --fix --json > "$target/.fix1.json" 2>/dev/null || true
    # Second run on now-healthy workspace.
    "$EE_BIN" doctor --workspace "$target" --fix --json > "$target/.fix2.json" 2>/dev/null || true

    # Both runs MUST emit the ee.doctor.fix_summary.v1 envelope; if the
    # CLI rejected the invocation (e.g., flag conflict) we'd see
    # ee.error.v2 and the jq fallback below would mistakenly read 0.
    # Defend against that by asserting the schema explicitly.
    fix1_schema=$(jq -r '.schema // ""' "$target/.fix1.json" 2>/dev/null)
    fix2_schema=$(jq -r '.schema // ""' "$target/.fix2.json" 2>/dev/null)
    if [ "$fix1_schema" != "ee.doctor.fix_summary.v1" ] || [ "$fix2_schema" != "ee.doctor.fix_summary.v1" ]; then
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id(schema_drift:$fix1_schema,$fix2_schema)"
        echo "verify-idempotence[$fm_id]: expected ee.doctor.fix_summary.v1, got run1=$fix1_schema run2=$fix2_schema" >&2
        continue
    fi

    # Second invocation must report zero new actions taken (re-running is a no-op).
    second_count=$(jq -r '.actionCount // .data.actionCount // ""' "$target/.fix2.json" 2>/dev/null || echo "")
    if [ "$second_count" = "0" ]; then
        PASS=$((PASS + 1))
    else
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id(count=$second_count)"
        echo "verify-idempotence[$fm_id]: expected actionCount=0 on second run, got '$second_count'" >&2
    fi
done
shopt -u nullglob

echo "verify-idempotence: passed=$PASS failed=$FAIL" >&2
if [ "$FAIL" -gt 0 ]; then
    echo "verify-idempotence: failed:$FAILED_FMS" >&2
    exit 1
fi
exit 0
