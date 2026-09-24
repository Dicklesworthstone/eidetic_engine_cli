#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# condition.sh <command> [args...] (contract in lib.sh, doctor_fixture_under_condition).
# The damage is a live writer holding .ee/ee.write.lock without progress. Start
# the holder corrupt.sh wrote, check it with the same witness assert.sh uses (a
# second non-blocking flock is refused), run the command, and stop the holder
# before returning, so no process outlives the run.
FM="fm-state_files-orphaned-pid-write-lock"
target="$(doctor_fixture_target)"
base="$target/.fixture_baseline"
lock="$target/.ee/ee.write.lock"
not_applied() {
    printf 'condition: %s not applied: %s\n' "$FM" "$1" >&2
    exit "$DOCTOR_FIXTURE_CONDITION_NOT_APPLIED"
}
[ -f "$lock" ] || not_applied "no $lock"
[ -f "$base/hold-write-lock.py" ] || not_applied "no holder script"
ready="$base/condition-lock-held.$$.$(date -u +%Y%m%dT%H%M%SZ)"
python3 "$base/hold-write-lock.py" "$lock" "$ready" 900 &
holder=$!
release() {
    kill "$holder" 2>/dev/null || true
    wait "$holder" 2>/dev/null || true
}
trap release EXIT
for _ in $(seq 1 200); do
    [ -f "$ready" ] && break
    kill -0 "$holder" 2>/dev/null || break
    sleep 0.05
done
[ -f "$ready" ] || not_applied "the holder never took the write lock"
[ "$(python3 "$base/probe-write-lock.py" "$lock")" = held ] || not_applied "the write lock is not held"
status=0
"$@" || status=$?
exit "$status"
