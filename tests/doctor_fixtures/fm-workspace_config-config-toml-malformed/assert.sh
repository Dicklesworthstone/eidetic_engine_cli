#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-workspace_config-config-toml-malformed"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# UPGRADED FROM A PINNED GAP TO A REAL REPAIR ROUND TRIP (bd-ooj2h, spec rule
# V6), same measured transition as its sibling
# fm-state_files-merge-conflict-markers. Oracle measured on an RCH worker
# 2026-10-14 against a binary that job built:
#
#   doctor            {"healthy":false,"posture":"blocked",
#                      "actionable":[{"name":"workspace","errorCode":"EE-E101"}]}
#   doctor --fix      {"status":"completed_ok","actionCount":1,
#                      "guidanceOnlyFixerCount":0,
#                      "fixers":[{"operation":"quarantine_by_rename",
#                                 "outcome":"applied"}]}
#   after --fix       {"healthy":true,"posture":"ok","actionable":[]}
#
# WORTH KNOWING BEFORE EDITING EITHER FIXTURE: doctor reports the two
# IDENTICALLY -- same check, same code, same posture, same repair string --
# because git conflict markers ARE invalid TOML, so both land in the same
# detector. These two fixtures therefore no longer exercise distinct doctor
# behaviour, and the difference that remains is only in how corrupt.sh
# produces the damage. bd-ooj2h records that as an open question rather than
# silently keeping two fixtures for one observable.
set +e
"$ee_bin" search "source memory" --workspace "$target" --json > "$base/search-assert.json" 2>&1
rc=$?
set -e
if [ "$rc" -eq 0 ] || ! jq -e '.schema == "ee.error.v2" and .error.code == "configuration"' \
    "$base/search-assert.json" >/dev/null; then
    printf 'fixture assert: %s witness lost: search exit %s without a configuration error\n' "$FM" "$rc" >&2
    exit 1
fi

"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-detect.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.healthy == false and .data.posture == "blocked" and
        any(.data.actionable[]; .name == "workspace" and .errorCode == "EE-E101"))
' "$base/doctor-detect.json" >/dev/null

doctor_fixture_assert "$FM" "P1" "workspace_config"

jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.status == "completed_ok" and .data.guidanceOnlyFixerCount == 0 and
        (.data.actionCount | type == "number" and . >= 1) and
        any(.data.fixerResults[];
            .operation == "quarantine_by_rename" and .outcome == "applied"))
' "$base/doctor-fix.json" >/dev/null

jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.status == "undone" and .data.firstError == null and
        (.data.actionsUndone | type == "number" and . > 0))
' "$base/doctor-undo.json" >/dev/null
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-after-undo.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and .data.healthy == false and
        any(.data.actionable[]; .name == "workspace" and .errorCode == "EE-E101"))
' "$base/doctor-after-undo.json" >/dev/null

printf 'real repair roundtrip confirmed: %s EE-E101 blocked -> quarantine_by_rename -> ok -> EE-E101 after undo\n' "$FM" >&2
