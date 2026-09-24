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

# GUIDANCE-ONLY since the bd-ixxzq fix: a live writer that holds the write lock
# and makes no progress is reported as database EE-E201 (locked), and --fix
# records database_locked guidance (wait for the writer; leave the lock file,
# database and sidecars in place). It used to be EE-E202 with the
# corrupted-store plan ("move .ee/ee.db aside and run ee init") for a store
# that is not damaged, and then EE-E207 (unavailable) after the first fix.
doctor_fixture_content_digest "$target" > "$base/locked-before.sha256"
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

"$ee_bin" doctor --workspace "$target" --json > "$base/locked-doctor.json"
if ! jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.healthy == false and .data.posture == "blocked" and
        any(.data.actionable[]; .name == "database" and .errorCode == "EE-E201" and
            (.message | test("write lock holder made no progress"))))
' "$base/locked-doctor.json" >/dev/null; then
    printf 'fixture assert: %s doctor does not report the held lock as database EE-E201; see %s\n' \
        "$FM" "$base/locked-doctor.json" >&2
    exit 1
fi
doctor_fixture_assert_guidance_only "$FM" "database_locked" "database" "EE-E201" \
    ".ee/index-rebuild-request.json"
# Lock guidance only: never the corrupted-store or unavailable plan.
if ! jq -es '
    length == 1 and (.[0] |
        all(.data.fixerResults[]; .operation == "manual" and .outcome == "guidance_recorded") and
        all(.data.fixerResults[]; .findingCode != "database_corrupted" and .findingCode != "database_unavailable"))
' "$base/doctor-fix.json" >/dev/null; then
    printf 'fixture assert: %s --fix recorded guidance other than database_locked; see %s\n' \
        "$FM" "$base/doctor-fix.json" >&2
    exit 1
fi
release
trap - EXIT

# The store was never damaged: once the holder is gone doctor is healthy and
# no byte outside doctor's own run records changed.
"$ee_bin" doctor --workspace "$target" --json > "$base/locked-released.json"
doctor_fixture_assert_health_report "$FM" "$base/locked-released.json"
doctor_fixture_content_digest "$target" > "$base/locked-after.sha256"
if ! cmp -s "$base/locked-before.sha256" "$base/locked-after.sha256"; then
    printf 'fixture assert: %s bytes changed while the lock was held\n' "$FM" >&2
    exit 1
fi
mv "$base/lock-held" "$base/lock-held.$(date -u +%Y%m%dT%H%M%SZ)"
printf 'guidance-only confirmed: %s (held lock reported as EE-E201, --fix recorded database_locked guidance only, store healthy after release)\n' \
    "$FM" >&2
