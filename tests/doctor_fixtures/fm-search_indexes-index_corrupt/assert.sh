#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-search_indexes-index_corrupt"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi

# REPAIR round trip: --fix rebuilds, doctor is healthy, undo restores the exact
# corrupted baseline (so EE-E301 returns), and a second undo is a no-op.
doctor_fixture_assert "$FM" "P0" "search_indexes"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.status == "completed_ok" and .data.guidanceOnlyFixerCount == 0 and
        any(.data.fixerResults[];
            .findingCode == "search_index_stale" and
            .operation == "run_index_rebuild" and .outcome == "applied"))
' "$base/doctor-fix.json" >/dev/null
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.status == "undone" and .data.firstError == null and
        (.data.actionsUndone | type == "number" and . > 0))
' "$base/doctor-undo.json" >/dev/null
rel="$(cat "$base/corrupted-index-file")"
test -f "$target/.ee/index/$rel"
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-after-undo.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and .data.healthy == false and
        any(.data.actionable[]; .name == "search_index" and .errorCode == "EE-E301"))
' "$base/doctor-after-undo.json" >/dev/null
run_id="$(jq -er '.data.runId' "$base/doctor-fix.json")"
"$ee_bin" doctor --workspace "$target" --undo "$run_id" --json > "$base/doctor-undo-again.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.status == "undone" and .data.actionsUndone == 0)
' "$base/doctor-undo-again.json" >/dev/null
doctor_fixture_content_digest "$target" > "$base/after-second-undo.sha256"
cmp "$base/before.sha256" "$base/after-second-undo.sha256"
printf 'real repair roundtrip confirmed: EE-E301 -> healthy -> EE-E301; exact baseline and idempotent undo\n' >&2
