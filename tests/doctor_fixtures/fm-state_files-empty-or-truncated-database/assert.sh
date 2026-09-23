#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-state_files-empty-or-truncated-database"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# PINNED DEFECT bd-xa6ud. Detection is asserted as it is today, and so is the
# crash: --fix dispatches the EE-E300 index repair against a database it cannot
# open and fails with doctor_runtime_io. When bd-xa6ud is fixed this assertion
# FAILS on purpose; upgrade the spec label and this fixture together (V6).
# NOT coverage.
#
# TARGET ORACLE once bd-xa6ud lands (orchestrator ruling on bd-xa6ud):
# GUIDANCE-ONLY -- a distinct finding (not EE-E700); posture blocked before AND
# after --fix; --fix exits 0 with guidance, no repair write and no migration;
# workspace bytes identical before and after; no crash. The workspace-bytes rule
# is already enforced below, around the pinned --fix.
before="$(doctor_fixture_sha256 "$target/.ee/ee.db")"
doctor_fixture_content_digest "$target" > "$base/pinned-before-fix.sha256"
"$ee_bin" doctor --workspace "$target" --json > "$base/pinned-doctor.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and .data.posture == "blocked" and
        any(.data.actionable[]; .name == "database" and .errorCode == "EE-E202") and
        any(.data.actionable[]; .name == "search_index" and .errorCode == "EE-E300"))
' "$base/pinned-doctor.json" >/dev/null
set +e
"$ee_bin" doctor --workspace "$target" --fix --json > "$base/pinned-fix.json" 2> "$base/pinned-fix.stderr"
rc=$?
set -e
if [ "$rc" -ne 3 ] || ! jq -es '
    length == 1 and (.[0] |
        .schema == "ee.error.v2" and .error.code == "doctor_runtime_io" and
        (.error.message | contains("build doctor index repair")))
' "$base/pinned-fix.json" >/dev/null; then
    printf 'fixture assert: %s pinned defect bd-xa6ud changed (--fix exit %s); upgrade the spec and fixture (V6); see %s\n' \
        "$FM" "$rc" "$base/pinned-fix.json" >&2
    exit 1
fi
test "$(doctor_fixture_sha256 "$target/.ee/ee.db")" = "$before"
doctor_fixture_content_digest "$target" > "$base/pinned-after-fix.sha256"
if ! cmp -s "$base/pinned-before-fix.sha256" "$base/pinned-after-fix.sha256"; then
    printf 'fixture assert: %s --fix changed workspace bytes while crashing\n' "$FM" >&2
    exit 1
fi
printf 'pinned defect confirmed: %s (EE-E202 blocked; --fix exit 3 doctor_runtime_io, bd-xa6ud) -- NOT coverage\n' "$FM" >&2
