#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# GUIDANCE-ONLY (bd-xa6ud fixed at 9ed78b70d; bd-2oh15 c9944): the real
# database is MOVED into the baseline and replaced by its own first 8192 bytes.
# doctor reports database and search_index EE-E202 (posture blocked).
FM="fm-state_files-empty-or-truncated-database"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

# Provision doctor's persistent lock through a healthy no-op run BEFORE the
# damage, so the pinned --fix below can be held to "workspace bytes identical"
# (bd-xa6ud ruling) without the lock's first creation counting as a change.
"$ee_bin" doctor --workspace "$target" --fix --json > "$base/doctor-initialize-lock.json"
jq -e '.schema == "ee.response.v2" and .success == true and .data.actionCount == 0' \
    "$base/doctor-initialize-lock.json" >/dev/null
test -f "$target/.ee/.doctor.lock"

mv "$target/.ee/ee.db" "$base/ee.db.original"
head -c 8192 "$base/ee.db.original" > "$target/.ee/ee.db"

doctor_fixture_corrupt "$FM" "P0" "state_files"
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-corrupt.json"
if ! jq -e '
    .schema == "ee.response.v2" and .success == true and
    .data.healthy == false and .data.posture == "blocked" and
    any(.data.actionable[]; .name == "database" and .errorCode == "EE-E202")
' "$base/doctor-corrupt.json" >/dev/null; then
    printf 'truncated-database fixture did not produce database EE-E202 / blocked; see %s\n' \
        "$base/doctor-corrupt.json" >&2
    exit 1
fi
printf 'real corruption confirmed: truncated database (8192 bytes, EE-E202, blocked)\n' >&2
