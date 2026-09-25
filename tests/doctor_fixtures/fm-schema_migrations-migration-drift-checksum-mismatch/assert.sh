#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-schema_migrations-migration-drift-checksum-mismatch"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# GUIDANCE-ONLY (bd-2oh15; bd-ixxzq): --fix records database_migration_drift
# guidance for the EE-E702 store. It used to record the corrupted-store plan
# (database_corrupted, EE-E202) for this intact store. It must still repair
# nothing: posture blocked before and after, every workspace byte unchanged,
# the tampered checksum still in place.
"$ee_bin" doctor --workspace "$target" --json > "$base/guidance-before.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and .data.posture == "blocked" and
        any(.data.actionable[]; .name == "database" and .errorCode == "EE-E702"))
' "$base/guidance-before.json" >/dev/null
doctor_fixture_content_digest "$target" > "$base/guidance-before-fix.sha256"
doctor_fixture_assert_guidance_only "$FM" "database_migration_drift" "database" "EE-E702" \
    ".ee/index-rebuild-request.json"
# bd-ixxzq: drift gets its own guidance, never the corrupted-store plan.
jq -es '
    length == 1 and (.[0] |
        .data.status == "completed_partial" and .data.fixerDispatchPending == true and
        all(.data.fixerResults[]; .operation == "manual" and .outcome == "guidance_recorded") and
        all(.data.fixerResults[]; .findingCode != "database_corrupted"))
' "$base/doctor-fix.json" >/dev/null
jq -e '.data.posture == "blocked"' "$base/doctor-after.json" >/dev/null
jq -e '[.data.coreChecks[]? | select(.name == "database") | .message] | any(contains("migration_drift"))' \
    "$base/doctor-after.json" >/dev/null
# Digest before the sqlite3 read below, which may itself create sidecars.
doctor_fixture_content_digest "$target" > "$base/guidance-after-fix.sha256"
if ! cmp -s "$base/guidance-before-fix.sha256" "$base/guidance-after-fix.sha256"; then
    printf 'fixture assert: %s guidance-only --fix changed workspace bytes\n' "$FM" >&2
    exit 1
fi
# The stored checksum stays tampered: nothing repaired it.
test "$(sqlite3 "$target/.ee/ee.db" "SELECT checksum FROM ee_schema_migrations ORDER BY version LIMIT 1;")" = \
    "blake3:0000000000000000000000000000000000000000000000000000000000000000"
