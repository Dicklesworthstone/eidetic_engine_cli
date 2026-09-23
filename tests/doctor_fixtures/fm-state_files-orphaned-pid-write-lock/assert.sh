#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-state_files-orphaned-pid-write-lock"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"
lock="$target/.ee/ee.write.lock"
test -f "$lock"
test -f "$base/lock-held" && { printf 'fixture assert: %s stale readiness file %s\n' "$FM" "$base/lock-held" >&2; exit 1; }

# PINNED DEFECT bd-ixxzq as it is today: a live writer that holds the write
# lock and makes no progress is reported as database EE-E202, and --fix
# records database_corrupted guidance (copy ee.db aside, restore a backup, or
# "move .ee/ee.db aside and run ee init") for a store that is not damaged.
# When bd-ixxzq gives the held lock its own finding, this fixture goes red on
# purpose and is relabelled.
doctor_fixture_content_digest "$target" > "$base/defect-before.sha256"
python3 "$base/hold-write-lock.py" "$lock" "$base/lock-held" 900 &
holder=$!
release() {
    kill "$holder" 2>/dev/null || true
    wait "$holder" 2>/dev/null || true
}
trap release EXIT
for _ in $(seq 1 200); do
    [ -f "$base/lock-held" ] && break
    kill -0 "$holder" 2>/dev/null || break
    sleep 0.05
done
if [ ! -f "$base/lock-held" ]; then
    printf 'fixture assert: %s holder never took the write lock\n' "$FM" >&2
    exit 1
fi
# Independent witness: while the holder lives, a second exclusive
# non-blocking flock on the same file is refused.
if [ "$(python3 "$base/probe-write-lock.py" "$lock")" != held ]; then
    printf 'fixture assert: %s witness lost: the write lock is not held\n' "$FM" >&2
    exit 1
fi

"$ee_bin" doctor --workspace "$target" --json > "$base/defect-doctor.json"
if ! jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.healthy == false and .data.posture == "blocked" and
        any(.data.actionable[]; .name == "database" and .errorCode == "EE-E202" and
            (.message | test("write lock holder made no progress"))))
' "$base/defect-doctor.json" >/dev/null; then
    printf 'fixture assert: %s doctor no longer reports the held lock as database EE-E202 (bd-ixxzq); relabel; see %s\n' \
        "$FM" "$base/defect-doctor.json" >&2
    exit 1
fi
fix_exit=0
"$ee_bin" doctor --workspace "$target" --fix --json > "$base/defect-fix.json" || fix_exit=$?
if [ "$fix_exit" -ne 6 ] || ! jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.status == "completed_partial" and
        any(.data.fixerResults[]; .findingCode == "database_corrupted" and
            .operation == "manual" and .outcome == "guidance_recorded") and
        all(.data.fixerResults[]; .outcome == "guidance_recorded"))
' "$base/defect-fix.json" >/dev/null; then
    printf 'fixture assert: %s --fix (exit %s) no longer records database_corrupted guidance for a held lock (bd-ixxzq); relabel; see %s\n' \
        "$FM" "$fix_exit" "$base/defect-fix.json" >&2
    exit 1
fi
release
trap - EXIT

# The store was never damaged: once the holder is gone doctor is healthy and
# no byte outside doctor's own run records changed.
"$ee_bin" doctor --workspace "$target" --json > "$base/defect-released.json"
doctor_fixture_assert_health_report "$FM" "$base/defect-released.json"
doctor_fixture_content_digest "$target" > "$base/defect-after.sha256"
if ! cmp -s "$base/defect-before.sha256" "$base/defect-after.sha256"; then
    printf 'fixture assert: %s bytes changed while the lock was held\n' "$FM" >&2
    exit 1
fi
mv "$base/lock-held" "$base/lock-held.$(date -u +%Y%m%dT%H%M%SZ)"
printf 'pinned defect confirmed: %s (held lock reported as EE-E202, --fix recorded database_corrupted guidance, store healthy after release) -- bd-ixxzq, NOT coverage\n' \
    "$FM" >&2
