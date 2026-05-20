#!/usr/bin/env bash
# scripts/verify-undo.sh — Phase 5 reversibility test for ee doctor.
#
# Per fixture under tests/doctor_fixtures/fm-*/, asserts the canonical
# round-trip: corrupt → ee doctor --fix → ee doctor --undo <run-id>
# → byte-identical to the corrupted baseline (excluding the doctor's own
# .doctor/ audit-trail directory, .fixture_baseline/, the harness's
# .assert.* capture files, and macOS ExFAT resource-fork sidecars).
# `--fix` and `--only` are mutually exclusive at the CLI; the per-FM
# scoping happens via the read-only diagnose call inside lib.sh::
# doctor_fixture_assert.
#
# Driven by scripts/run-safety-harness.sh (bd-21joy stage 8.5 of
# scripts/verify.sh). Source: world-class-doctor-mode skill, adapted to
# ee's actual CLI surface (--fix and --undo are FLAGS, not subcommands).
#
# Exits 0 on full pass across the fixture set, 1 on first failure,
# 64 on usage error. Honors EE_DOCTOR_FIXTURE_ROOT and EE_DOCTOR_FIXTURE_BINARY.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURE_ROOT="${EE_DOCTOR_FIXTURE_ROOT:-${TMPDIR:-/tmp}/ee-doctor-fixtures}"
FIXTURES_SRC="${EE_DOCTOR_FIXTURES_SRC:-$REPO_ROOT/tests/doctor_fixtures}"
EE_BIN="${EE_DOCTOR_FIXTURE_BINARY:-ee}"

if ! command -v "$EE_BIN" >/dev/null 2>&1; then
    echo "verify-undo: ee binary '$EE_BIN' not on PATH; skipping (advisory)" >&2
    exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "verify-undo: jq required" >&2
    exit 64
fi

# Build the fixture set under FIXTURE_ROOT by calling each fm-*/corrupt.sh
# (which writes a marker into <target>/.ee/doctor-fixtures/<fm>.json and
# a baseline SHA-256 manifest into <target>/.fixture_baseline/before.sha256).
mkdir -p "$FIXTURE_ROOT"
PASS=0
FAIL=0
FAILED_FMS=""

shopt -s nullglob
for fm_dir in "$FIXTURES_SRC"/fm-*; do
    fm_id="$(basename "$fm_dir")"
    target="$FIXTURE_ROOT/$fm_id"
    mkdir -p "$target"

    # Build corrupted state.
    if ! EE_DOCTOR_FIXTURE_TARGET="$target" "$fm_dir/corrupt.sh" >/dev/null 2>&1; then
        echo "verify-undo[$fm_id]: corrupt.sh failed; skipping" >&2
        continue
    fi

    # Run the assertion in EE_DOCTOR_FIXTURE_RUN_EE=1 mode which does the
    # full --fix → --undo round-trip via lib.sh::doctor_fixture_assert.
    if EE_DOCTOR_FIXTURE_TARGET="$target" \
       EE_DOCTOR_FIXTURE_RUN_EE=1 \
       EE_DOCTOR_FIXTURE_BINARY="$EE_BIN" \
       "$fm_dir/assert.sh" >"$target/.assert.stdout" 2>"$target/.assert.stderr"; then
        PASS=$((PASS + 1))
    else
        FAIL=$((FAIL + 1))
        FAILED_FMS="$FAILED_FMS $fm_id"
        echo "verify-undo[$fm_id]: assert FAILED" >&2
        tail -20 "$target/.assert.stderr" >&2 || true
    fi
done
shopt -u nullglob

echo "verify-undo: passed=$PASS failed=$FAIL fixture_root=$FIXTURE_ROOT" >&2
if [ "$FAIL" -gt 0 ]; then
    echo "verify-undo: failed FMs:$FAILED_FMS" >&2
    exit 1
fi
exit 0
