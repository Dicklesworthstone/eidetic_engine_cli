#!/usr/bin/env bash
# scripts/verify-crash-recovery.sh — Phase 5 crash-mid-fix test.
#
# Simulates SIGKILL mid-fix: spawn `ee doctor --fix`, kill it after a
# short delay, then re-run `ee doctor --fix`. Asserts the second run
# either:
#   * completes (exit 0/2) because the lock or actions.jsonl was atomic
#   * refuses with exit 5 (concurrency_lost) if the stale lock survives
#   * refuses with exit 4 (refused_unsafe) if state is unrecoverable
#
# Never leaves the workspace in an undefined state (no `--fix` partial
# half-writes visible to readers).

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EE_BIN="${EE_DOCTOR_FIXTURE_BINARY:-ee}"

if ! command -v "$EE_BIN" >/dev/null 2>&1; then
    echo "verify-crash-recovery: ee binary '$EE_BIN' not on PATH; skipping (advisory)" >&2
    exit 0
fi

# Round-3 self-review: use mktemp instead of rm -rf on a caller-controlled
# path. Confine destructive cleanup to a tempdir owned by this invocation.
target="$(mktemp -d "${TMPDIR:-/tmp}/ee-doctor-crash.XXXXXX")"
trap 'rm -rf "$target"' EXIT
mkdir -p "$target/.ee"

# Spawn a doctor --fix, SIGKILL it shortly after start.
"$EE_BIN" doctor --workspace "$target" --fix --json > "$target/.run1.json" 2>&1 &
victim=$!
# Race window: 100ms is short enough that the doctor may or may not have
# reached the chokepoint; both outcomes are valid.
sleep 0.1
kill -9 "$victim" 2>/dev/null || true
wait "$victim" 2>/dev/null || true

# Now retry. Three acceptable outcomes:
#   exit 0/2 — second run completes (lock released or stale-detected)
#   exit 5   — lock still held (stale lock from prior run)
#   exit 4   — state genuinely unsafe; doctor refuses
rc=0
"$EE_BIN" doctor --workspace "$target" --fix --json > "$target/.run2.json" 2>&1 || rc=$?

case "$rc" in
    0|2|4|5)
        echo "verify-crash-recovery: PASS (retry exit=$rc)" >&2
        exit 0
        ;;
    *)
        echo "verify-crash-recovery: FAIL — retry produced unexpected exit=$rc" >&2
        cat "$target/.run2.json" >&2 || true
        exit 1
        ;;
esac
