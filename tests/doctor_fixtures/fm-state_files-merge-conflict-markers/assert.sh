#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-state_files-merge-conflict-markers"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# UPGRADED FROM A PINNED GAP TO A REAL REPAIR ROUND TRIP (bd-ooj2h, spec rule
# V6). This fixture used to assert doctor was BLIND to conflict markers in
# .ee/config.toml -- healthy, posture ok, --fix taking no action. That label
# stopped being true: the harness started failing with "GAP no longer
# reproduces; the label is wrong, relabel it", which is the pinned-gap
# assertion doing exactly what its comment promised on the day doctor learned
# to detect this.
#
# The oracle below was MEASURED on an RCH worker 2026-10-14 against a binary
# that job built, not inferred from what doctor ought to do:
#
#   doctor            {"healthy":false,"posture":"blocked",
#                      "actionable":[{"name":"workspace","errorCode":"EE-E101",
#                      "severity":"error","repair":"Fix the TOML syntax in the
#                      config file the error names, then re-run the command"}]}
#   doctor --fix      {"status":"completed_ok","actionCount":1,
#                      "fixerDispatchPending":false,"guidanceOnlyFixerCount":0,
#                      "fixers":[{"operation":"quarantine_by_rename",
#                                 "outcome":"applied"}]}
#   after --fix       {"healthy":true,"posture":"ok","actionable":[]}
#   artifact          .ee/config.toml is GONE afterwards (quarantined by
#                     rename, never deleted)

# Independent witness before the repair: a real command still fails on the
# conflicted config, so the damage is real rather than merely marked.
set +e
"$ee_bin" search "source memory" --workspace "$target" --json > "$base/search-assert.json" 2>&1
rc=$?
set -e
if [ "$rc" -eq 0 ] || ! jq -e '.schema == "ee.error.v2" and .error.code == "configuration"' \
    "$base/search-assert.json" >/dev/null; then
    printf 'fixture assert: %s witness lost: search exit %s without a configuration error\n' "$FM" "$rc" >&2
    exit 1
fi

# Detection, before any repair runs.
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-detect.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.healthy == false and .data.posture == "blocked" and
        any(.data.actionable[]; .name == "workspace" and .errorCode == "EE-E101"))
' "$base/doctor-detect.json" >/dev/null

# The round trip: --fix, a healthy report, --undo, and a byte-identical
# restoration of the DAMAGED baseline (doctor_fixture_corrupt wrote
# before.sha256 after the damage, so undo must bring the damage back).
doctor_fixture_assert "$FM" "P1" "state_files"

# The repair is a QUARANTINE, not a delete. RULE 1 and the doctor runtime
# contract both require that doctor never removes a file, so assert the
# operation by name rather than just that something was applied.
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.status == "completed_ok" and .data.guidanceOnlyFixerCount == 0 and
        (.data.actionCount | type == "number" and . >= 1) and
        any(.data.fixerResults[];
            .operation == "quarantine_by_rename" and .outcome == "applied"))
' "$base/doctor-fix.json" >/dev/null

# Undo reported work, and the damage is back: doctor reports EE-E101 again.
# Without this the digest comparison inside doctor_fixture_assert could be
# satisfied by a workspace that was never repaired in the first place.
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
