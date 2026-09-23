#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-state_files-jsonl-tombstone-drift"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"
gone="$(cat "$base/tombstoned-id")"

# Independent witness, read-only: the tracker database holds the tombstone
# while the export still carries the same issue as live.
db_status="$(sqlite3 -readonly "$target/.beads/beads.db" "SELECT status FROM issues WHERE id = '$gone';")"
if [ "$db_status" != tombstone ] || ! jq -es --arg id "$gone" \
    'map(select(.id == $id)) | length == 1 and .[0].status != "tombstone"' \
    "$target/.beads/issues.jsonl" >/dev/null; then
    printf 'fixture assert: %s witness lost: db status %s for %s, or the export carries the tombstone again\n' \
        "$FM" "$db_status" "$gone" >&2
    exit 1
fi
doctor_fixture_assert_pinned_gap "$FM" ".beads/issues.jsonl"
