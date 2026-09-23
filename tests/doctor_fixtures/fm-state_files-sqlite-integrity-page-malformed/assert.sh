#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-state_files-sqlite-integrity-page-malformed"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# Independent witness: SQLite itself says the database is malformed.
sqlite3 "$target/.ee/ee.db" 'PRAGMA integrity_check;' > "$base/integrity-assert.txt" 2>&1 || true
if [ "$(head -n 1 "$base/integrity-assert.txt")" = "ok" ]; then
    printf 'fixture assert: %s witness lost: integrity_check reports ok\n' "$FM" >&2
    exit 1
fi
doctor_fixture_assert_pinned_gap "$FM" ".ee/ee.db"
