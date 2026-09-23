#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# GUIDANCE-ONLY (bd-2oh15 survey c9983; measured on a stamped 02524267e build):
# the newest row of ee_schema_migrations is removed with SQL, so the store
# reports a migration the binary expects as not applied. doctor's database
# check reports EE-E700 (posture degraded_recoverable); --fix records
# run_migration guidance only and exits 6. A byte copy of the database from
# before the edit is kept in the baseline.
FM="fm-schema_migrations-pending-migrations-detected"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
command -v sqlite3 >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"
# Provision doctor's persistent lock through a healthy no-op run BEFORE the
# edit, so the guidance-only --fix can be held to "workspace bytes identical"
# without the lock's first creation counting as a change.
"$ee_bin" doctor --workspace "$target" --fix --json > "$base/doctor-initialize-lock.json"
jq -e '.schema == "ee.response.v2" and .success == true and .data.actionCount == 0' \
    "$base/doctor-initialize-lock.json" >/dev/null
test -f "$target/.ee/.doctor.lock"

cp -p "$target/.ee/ee.db" "$base/ee.db.pre-edit"
sqlite3 "$target/.ee/ee.db" "SELECT count(*) FROM ee_schema_migrations;" > "$base/ledger-rows.before"
sqlite3 "$target/.ee/ee.db" \
    "DELETE FROM ee_schema_migrations WHERE version = (SELECT MAX(version) FROM ee_schema_migrations);"
sqlite3 "$target/.ee/ee.db" "SELECT count(*) FROM ee_schema_migrations;" > "$base/ledger-rows.after-edit"
test "$(cat "$base/ledger-rows.after-edit")" -eq "$(( $(cat "$base/ledger-rows.before") - 1 ))"

doctor_fixture_corrupt "$FM" "P1" "schema_migrations"
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-corrupt.json"
if ! jq -e '
    .schema == "ee.response.v2" and .success == true and
    any(.data.actionable[]; .name == "database" and .errorCode == "EE-E700")
' "$base/doctor-corrupt.json" >/dev/null; then
    printf 'pending-migrations fixture did not produce database EE-E700; see %s\n' \
        "$base/doctor-corrupt.json" >&2
    exit 1
fi
printf 'real state confirmed: newest migration row removed (EE-E700 pending migration)\n' >&2
