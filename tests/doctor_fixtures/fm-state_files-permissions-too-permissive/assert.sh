#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-state_files-permissions-too-permissive"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# The mode string from ls (e.g. -rw-r--r--) is portable across GNU and BSD.
# exposed <path>: group AND other can read it (and, for a directory, enter it).
exposed() {
    local mode
    mode="$(command ls -ld -- "$1" | cut -c1-10)"
    [ "${mode:4:1}" = r ] && [ "${mode:7:1}" = r ] || return 1
    if [ -d "$1" ]; then
        [ "${mode:6:1}" = x ] && [ "${mode:9:1}" = x ]
    fi
}

# Independent witness: the store and its directory are group/other readable.
if ! exposed "$target/.ee/ee.db" || ! exposed "$target/.ee"; then
    printf 'fixture assert: %s witness lost: .ee or ee.db is no longer group/other readable\n' "$FM" >&2
    exit 1
fi
before_bytes="$(doctor_fixture_sha256 "$target/.ee/ee.db")"
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-detect.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and .data.healthy == false and
        any(.data.actionable[]; .name == "state_directory_permissions" and .errorCode == "EE-E208") and
        any(.data.actionable[]; .name == "state_file_permissions" and .errorCode == "EE-E208"))
' "$base/doctor-detect.json" >/dev/null
# Read-only diagnosis preserves both the exposure witness and database bytes.
exposed "$target/.ee/ee.db"
exposed "$target/.ee"
[ "$(doctor_fixture_sha256 "$target/.ee/ee.db")" = "$before_bytes" ]

"$ee_bin" doctor --workspace "$target" --fix --json > "$base/doctor-fix.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and .data.status == "completed_ok" and
        .data.unresolvedCoreCheckCount == 0 and
        any(.data.fixerResults[]; .findingCode == "state_directory_permission_drift" and .operation == "chmod" and .outcome == "applied") and
        any(.data.fixerResults[]; .findingCode == "state_file_permission_drift" and .operation == "chmod" and .outcome == "applied"))
' "$base/doctor-fix.json" >/dev/null
[ "$(command ls -ld -- "$target/.ee" | cut -c1-10)" = "drwx------" ]
[ "$(command ls -ld -- "$target/.ee/ee.db" | cut -c1-10)" = "-rw-------" ]
[ "$(doctor_fixture_sha256 "$target/.ee/ee.db")" = "$before_bytes" ]
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-after.json"
doctor_fixture_assert_health_report "$FM" "$base/doctor-after.json"

"$ee_bin" doctor --workspace "$target" --fix --json > "$base/doctor-fix-again.json"
jq -e '
    .success == true and all(.data.fixerResults[];
        .findingCode != "state_directory_permission_drift" and .findingCode != "state_file_permission_drift")
' "$base/doctor-fix-again.json" >/dev/null

run_id="$(jq -er '.data.runId' "$base/doctor-fix.json")"
"$ee_bin" doctor --workspace "$target" --undo "$run_id" --json > "$base/doctor-undo.json"
jq -e '
    .success == true and .data.status == "undone" and .data.firstError == null and .data.actionsUndone >= 2
' "$base/doctor-undo.json" >/dev/null
[ "$(command ls -ld -- "$target/.ee" | cut -c1-10)" = "drwxr-xr-x" ]
[ "$(command ls -ld -- "$target/.ee/ee.db" | cut -c1-10)" = "-rw-r--r--" ]
[ "$(doctor_fixture_sha256 "$target/.ee/ee.db")" = "$before_bytes" ]
doctor_fixture_content_digest "$target" > "$base/after-undo.sha256"
doctor_fixture_content_manifest "$target" > "$base/after-undo.manifest"
cmp "$base/before.sha256" "$base/after-undo.sha256"
doctor_fixture_assert_write_lock_monotonic "$FM" "$target"
doctor_fixture_assert_doctor_lock_persistent "$FM" "$target"
exposed "$target/.ee/ee.db"
exposed "$target/.ee"
printf 'permission repair roundtrip confirmed: 0755/0644 -> 0700/0600 -> exact original modes, database bytes unchanged\n' >&2
