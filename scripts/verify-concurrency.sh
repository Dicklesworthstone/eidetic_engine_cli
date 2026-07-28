#!/usr/bin/env bash
# scripts/verify-concurrency.sh — Phase 5 concurrency-safety test.
#
# Holds the exact persistent doctor lock with the platform `flock` primitive,
# then launches `ee doctor --fix` against the same workspace. Asserts the CLI
# refuses with its typed concurrency error rather than mutating concurrently.
#
# Picks a representative fixture (fm-state_files-permissions-too-permissive
# is auto-fixable through Chmod and doesn't need subsystem actor handles).
# Driven by scripts/run-safety-harness.sh.

set -euo pipefail
EE_BIN="${EE_DOCTOR_FIXTURE_BINARY:-ee}"

if ! command -v "$EE_BIN" >/dev/null 2>&1; then
    echo "verify-concurrency: ee binary '$EE_BIN' not on PATH; skipping (advisory)" >&2
    exit 0
fi
if ! command -v flock >/dev/null 2>&1; then
    echo "verify-concurrency: flock not available; skipping external-holder probe (advisory)" >&2
    exit 0
fi

# A normal `--fix` finishes too quickly for a process race to be deterministic.
# Hold the same kernel advisory lock used by `RunContext` instead. Merely
# creating the persistent file is intentionally not contention anymore.
target="$(mktemp -d "${TMPDIR:-/tmp}/ee-doctor-conc.XXXXXX")"
holder_pid=""
# shellcheck disable=SC2329  # invoked indirectly by the EXIT trap
cleanup() {
    if [ -n "$holder_pid" ] && kill -0 "$holder_pid" 2>/dev/null; then
        kill "$holder_pid" 2>/dev/null || true
        wait "$holder_pid" 2>/dev/null || true
    fi
    rm -rf "$target"
}
trap cleanup EXIT
mkdir -p "$target/.ee"

lock_path="$target/.ee/.doctor.lock"
ready_path="$target/.holder-ready"
(
    exec 9<>"$lock_path"
    flock -n 9
    printf 'verify-concurrency-holder\n%d\n' "$$" >&9
    : > "$ready_path"
    sleep 30
) &
holder_pid=$!

for _attempt in {1..100}; do
    if [ -f "$ready_path" ]; then
        break
    fi
    if ! kill -0 "$holder_pid" 2>/dev/null; then
        echo "verify-concurrency: external holder exited before acquiring the lock" >&2
        exit 1
    fi
    sleep 0.05
done
if [ ! -f "$ready_path" ]; then
    echo "verify-concurrency: timed out waiting for external holder" >&2
    exit 1
fi

# Run --fix; expect a typed nonzero refusal while the holder remains live.
set +e
"$EE_BIN" doctor --workspace "$target" --fix --json > "$target/.run.json" 2>&1
rc=$?
set -e
echo "verify-concurrency: rc=$rc" >&2

# The machine contract is authoritative; the nonzero exit is independently
# required so shell callers cannot mistake the refusal for success.
schema=$(jq -r '.schema // ""' "$target/.run.json" 2>/dev/null || echo "")
code=$(jq -r '.error.code // ""' "$target/.run.json" 2>/dev/null || echo "")
phase=$(jq -r '.error.details.phase // ""' "$target/.run.json" 2>/dev/null || echo "")

if [ "$rc" -ne 0 ] &&
   [ "$schema" = "ee.error.v2" ] &&
   [ "$code" = "doctor_concurrency_lost" ] &&
   [ "$phase" = "start" ]; then
    echo "verify-concurrency: PASS (typed lock refusal, rc=$rc)" >&2
    exit 0
fi

# If the JSON or process status did not carry the expected refusal, fail.
echo "verify-concurrency: FAIL — expected nonzero typed lock refusal" >&2
echo "verify-concurrency: actual rc=$rc schema=$schema code=$code phase=$phase" >&2
cat "$target/.run.json" >&2
exit 1
