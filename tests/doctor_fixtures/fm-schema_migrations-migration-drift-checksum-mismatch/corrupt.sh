#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# GUIDANCE-ONLY (bd-2oh15 c9853; guidance since 9ed78b70d): the stored checksum
# of the first applied migration is changed with SQL. doctor's database check
# reports EE-E202 with an EE-E040 migration_drift message (posture blocked);
# --fix records manual guidance only. A byte copy of the pre-drift database is
# kept in the baseline.
FM="fm-schema_migrations-migration-drift-checksum-mismatch"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
command -v sqlite3 >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"
# Provision doctor's persistent lock through a healthy no-op run BEFORE the
# drift, so the guidance-only --fix can be held to "workspace bytes identical"
# without the lock's first creation counting as a change (measured at
# a2c2f950f: that creation was the only byte change --fix made).
"$ee_bin" doctor --workspace "$target" --fix --json > "$base/doctor-initialize-lock.json"
jq -e '.schema == "ee.response.v2" and .success == true and .data.actionCount == 0' \
    "$base/doctor-initialize-lock.json" >/dev/null
test -f "$target/.ee/.doctor.lock"

cp -p "$target/.ee/ee.db" "$base/ee.db.pre-drift"
sqlite3 "$target/.ee/ee.db" \
    "SELECT version, name, checksum FROM ee_schema_migrations ORDER BY version LIMIT 1;" \
    > "$base/migration-row.before"
sqlite3 "$target/.ee/ee.db" \
    "UPDATE ee_schema_migrations SET checksum = 'blake3:0000000000000000000000000000000000000000000000000000000000000000' WHERE version = (SELECT MIN(version) FROM ee_schema_migrations);"

doctor_fixture_corrupt "$FM" "P0" "schema_migrations"
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-corrupt.json"
if ! jq -e '
    .schema == "ee.response.v2" and .success == true and .data.posture == "blocked" and
    any(.data.actionable[]; .name == "database" and .errorCode == "EE-E202")
' "$base/doctor-corrupt.json" >/dev/null; then
    printf 'migration-drift fixture did not produce database EE-E202 / blocked; see %s\n' \
        "$base/doctor-corrupt.json" >&2
    exit 1
fi
printf 'real corruption confirmed: migration checksum drift (EE-E202, blocked)\n' >&2
