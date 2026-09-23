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
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

doctor_fixture_assert_report_only "$FM" "database" "EE-E202" concise "-"
# The stored checksum stays tampered: report-only means nothing repaired it.
test "$(sqlite3 "$target/.ee/ee.db" "SELECT checksum FROM ee_schema_migrations ORDER BY version LIMIT 1;")" = \
    "blake3:0000000000000000000000000000000000000000000000000000000000000000"
jq -e '[.data.coreChecks[]? | select(.name == "database") | .message] | any(contains("migration_drift"))' \
    "$base/report-after.json" >/dev/null
