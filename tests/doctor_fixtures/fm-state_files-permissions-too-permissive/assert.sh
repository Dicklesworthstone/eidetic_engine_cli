#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-state_files-permissions-too-permissive"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"

# Independent witness: the store and its directory are group/other readable.
if [ -z "$(find "$target/.ee/ee.db" -perm -044)" ] || [ -z "$(find "$target/.ee" -maxdepth 0 -perm -055)" ]; then
    printf 'fixture assert: %s witness lost: .ee or ee.db is no longer group/other readable\n' "$FM" >&2
    exit 1
fi
doctor_fixture_assert_pinned_gap "$FM" ".ee/ee.db"
# Nothing tightened the modes: the gap is still there after --fix.
test -n "$(find "$target/.ee/ee.db" -perm -044)"
test -n "$(find "$target/.ee" -maxdepth 0 -perm -055)"
