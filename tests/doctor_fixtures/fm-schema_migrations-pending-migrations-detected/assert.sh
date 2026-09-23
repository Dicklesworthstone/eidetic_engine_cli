#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-schema_migrations-pending-migrations-detected"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# GUIDANCE-ONLY: --fix records run_migration guidance for the pending migration
# and must run no migration: EE-E700 before and after, every workspace byte
# unchanged, and the ledger still one row short.
"$ee_bin" doctor --workspace "$target" --json > "$base/guidance-before.json"
jq -es '
    length == 1 and (.[0] |
        .schema == "ee.response.v2" and .success == true and
        any(.data.actionable[]; .name == "database" and .errorCode == "EE-E700"))
' "$base/guidance-before.json" >/dev/null
doctor_fixture_content_digest "$target" > "$base/guidance-before-fix.sha256"
doctor_fixture_assert_guidance_only "$FM" "schema_migration_pending" "database" "EE-E700" \
    ".ee/index-rebuild-request.json"
jq -es '
    length == 1 and (.[0] |
        .data.status == "completed_partial" and .data.fixerDispatchPending == true and
        all(.data.fixerResults[]; .operation == "run_migration" and .outcome == "guidance_recorded"))
' "$base/doctor-fix.json" >/dev/null
# Digest before the sqlite3 read below, which may itself create sidecars.
doctor_fixture_content_digest "$target" > "$base/guidance-after-fix.sha256"
if ! cmp -s "$base/guidance-before-fix.sha256" "$base/guidance-after-fix.sha256"; then
    printf 'fixture assert: %s guidance-only --fix changed workspace bytes\n' "$FM" >&2
    exit 1
fi
# No migration ran: the ledger is still one row short.
test "$(sqlite3 "$target/.ee/ee.db" "SELECT count(*) FROM ee_schema_migrations;")" = \
    "$(cat "$base/ledger-rows.after-edit")"
