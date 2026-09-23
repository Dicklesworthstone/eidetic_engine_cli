#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# NOT-DETECTED, PINNED GAP (bd-2oh15 survey c9983): `ee init` creates .ee as
# 0700 and ee.db as 0600. Here a healthy store is opened up to group and other
# (.ee 0755, ee.db 0644), so any local user can read the memory store. No
# doctor check reads file modes, and the chmod fixer is never dispatched.
FM="fm-state_files-permissions-too-permissive"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

# The baseline modes must be the private ones init sets; otherwise the fixture
# would not be opening anything up.
if [ -n "$(find "$target/.ee/ee.db" -perm -044)" ] || [ -n "$(find "$target/.ee" -maxdepth 0 -perm -055)" ]; then
    printf 'permissions fixture: the healthy store is already group/other readable\n' >&2
    exit 1
fi
chmod 0755 "$target/.ee"
chmod 0644 "$target/.ee/ee.db"

doctor_fixture_corrupt "$FM" "P1" "state_files"
test -n "$(find "$target/.ee/ee.db" -perm -044)"
test -n "$(find "$target/.ee" -maxdepth 0 -perm -055)"
printf 'real exposure confirmed: .ee is 0755 and ee.db is 0644 (group/other readable)\n' >&2
