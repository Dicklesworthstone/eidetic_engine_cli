#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf 'index_missing assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' >&2
    exit 2
fi

# Repair is now derived-only: no SQLite job, model-registry or anchor writes.
# Require real post-fix health and the existing byte/path exact undo baseline;
# a guidance-only receipt, a no-op repair, or source drift must fail this test.
doctor_fixture_assert "fm-search_indexes-index_missing" "P1" "search_indexes"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
fix="$target/.fixture_baseline/doctor-fix.json"
undo="$target/.fixture_baseline/doctor-undo.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.status == "completed_ok" and .data.guidanceOnlyFixerCount == 0 and
        (.data.actionCount | type == "number" and . > 2) and
        any(.data.fixerResults[];
            .findingCode == "search_index_missing" and
            .operation == "run_index_rebuild" and .outcome == "applied" and
            (.actionSequence | type == "number" and . > 2)))
' "$fix" >/dev/null
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.status == "undone" and .data.firstError == null and
        (.data.actionsUndone | type == "number" and . > 2))
' "$undo" >/dev/null

test ! -e "$target/.ee/index"
test ! -L "$target/.ee/index"
test -f "$target/.ee/ee.db"
test -f "$target/.ee/.doctor.lock"
"$ee_bin" doctor --workspace "$target" --json \
    > "$target/.fixture_baseline/doctor-after-undo.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.healthy == false and
        any(.data.actionable[]; .name == "search_index" and .errorCode == "EE-E300"))
' "$target/.fixture_baseline/doctor-after-undo.json" >/dev/null
run_id="$(jq -er '.data.runId' "$fix")"
"$ee_bin" doctor --workspace "$target" --undo "$run_id" --json \
    > "$target/.fixture_baseline/doctor-undo-again.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        .data.status == "undone" and .data.actionsUndone == 0)
' "$target/.fixture_baseline/doctor-undo-again.json" >/dev/null
doctor_fixture_content_digest "$target" > "$target/.fixture_baseline/after-second-undo.sha256"
cmp "$target/.fixture_baseline/before.sha256" "$target/.fixture_baseline/after-second-undo.sha256"
doctor_fixture_assert_write_lock_monotonic "fm-search_indexes-index_missing" "$target"
printf 'real repair roundtrip confirmed: EE-E300 -> healthy -> EE-E300; exact source baseline and idempotent undo\n' >&2
