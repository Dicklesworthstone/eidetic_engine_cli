#!/usr/bin/env bash
# scripts/verify-crash-recovery.sh — Phase 5 crash-mid-fix test.
#
# Simulates SIGKILL mid-fix: spawn `ee doctor --fix`, kill it after a
# short delay, then re-run `ee doctor --fix`. Asserts the second run
# either:
#   * completes (exit 0/2) because the lock or actions.jsonl was atomic
#   * refuses with exit 5 (concurrency_lost) if the stale lock survives
#   * refuses with exit 4 (refused_unsafe) if state is unrecoverable
#   * exits 6 in the ONE admissible guidance-only state (bd-2oh15 ruling
#     c10026): an ee.response.v2 success envelope with no error, every
#     data.unresolvedCoreChecks entry guidance-class (fixMode auto_guidance or
#     manual), every fixer receipt guidance_recorded, and the store bytes
#     unchanged by the retry. Any other exit 6 fails.
#
# Never leaves the workspace in an undefined state (no `--fix` partial
# half-writes visible to readers). The workspace and the run outputs are kept
# for inspection; nothing is removed.

set -euo pipefail
EE_BIN="${EE_DOCTOR_FIXTURE_BINARY:-ee}"

if ! command -v "$EE_BIN" >/dev/null 2>&1; then
    # A harness that ran nothing is not a pass (bd-2oh15 ruling, option a).
    echo "verify-crash-recovery: SKIPPED: ee binary '$EE_BIN' not found; set EE_DOCTOR_FIXTURE_BINARY" >&2
    exit 3
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "verify-crash-recovery: jq required" >&2
    exit 64
fi

# The workspace under test, and a separate directory for the run outputs so
# they are never part of the store digest.
target="$(mktemp -d "${TMPDIR:-/tmp}/ee-doctor-crash.XXXXXX")"
work="$(mktemp -d "${TMPDIR:-/tmp}/ee-doctor-crash-work.XXXXXX")"
mkdir -p "$target/.ee"
echo "verify-crash-recovery: workspace=$target outputs=$work" >&2

# Store bytes: every regular file except doctor's own run directory and
# coordination files (.doctor/, .doctor.lock, ee.write.lock) and the transient
# ee.db-shm.
store_digest() {
    find "$1" -type f \
        -not -path '*/.doctor/*' \
        -not -name '.doctor.lock' \
        -not -name 'ee.write.lock' \
        -not -name 'ee.db-shm' \
        -exec shasum -a 256 -- {} + | LC_ALL=C sort | shasum -a 256
}

# Spawn a doctor --fix, SIGKILL it shortly after start.
"$EE_BIN" doctor --workspace "$target" --fix --json > "$work/run1.json" 2> "$work/run1.stderr" &
victim=$!
# Race window: 100ms is short enough that the doctor may or may not have
# reached the chokepoint; both outcomes are valid.
sleep 0.1
kill -9 "$victim" 2>/dev/null || true
wait "$victim" 2>/dev/null || true

# Now retry, digesting the store around it.
store_digest "$target" > "$work/store-before-retry.sha256"
rc=0
"$EE_BIN" doctor --workspace "$target" --fix --json > "$work/run2.json" 2> "$work/run2.stderr" || rc=$?
store_digest "$target" > "$work/store-after-retry.sha256"

case "$rc" in
    0|2|4|5)
        echo "verify-crash-recovery: PASS (retry exit=$rc)" >&2
        exit 0
        ;;
    6)
        if ! jq -es '
            length == 1 and (.[0] |
                .schema == "ee.response.v2" and .success == true and (has("error") | not) and
                (.data.unresolvedCoreChecks | type == "array" and length > 0 and
                    all(.[]; .fixMode == "auto_guidance" or .fixMode == "manual")) and
                all(.data.fixerResults[]?; .outcome == "guidance_recorded"))
        ' "$work/run2.json" >/dev/null 2>&1; then
            echo "verify-crash-recovery: FAIL — retry exit=6 outside the admissible guidance-only state (error envelope, a non-guidance unresolved check, or a non-guidance fixer receipt)" >&2
            cat "$work/run2.json" >&2 || true
            exit 1
        fi
        if ! cmp -s "$work/store-before-retry.sha256" "$work/store-after-retry.sha256"; then
            echo "verify-crash-recovery: FAIL — retry exit=6 changed store bytes" >&2
            exit 1
        fi
        echo "verify-crash-recovery: PASS (retry exit=6, guidance-only, store bytes unchanged)" >&2
        exit 0
        ;;
    *)
        echo "verify-crash-recovery: FAIL — retry produced unexpected exit=$rc" >&2
        cat "$work/run2.json" >&2 || true
        exit 1
        ;;
esac
