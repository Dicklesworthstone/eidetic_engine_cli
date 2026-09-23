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

# The mode string from ls (e.g. -rw-r--r--) is portable across GNU and BSD.
# exposed <path>: group AND other can read it (and, for a directory, enter it).
exposed() {
    local mode
    mode="$(command ls -ld -- "$1" | cut -c1-10)"
    [ "${mode:4:1}" = r ] && [ "${mode:7:1}" = r ] || return 1
    if [ -d "$1" ]; then
        [ "${mode:6:1}" = x ] && [ "${mode:9:1}" = x ]
    fi
}

doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"

# The baseline must be private as init leaves it; otherwise the fixture would
# not be opening anything up.
if exposed "$target/.ee/ee.db" || exposed "$target/.ee"; then
    printf 'permissions fixture: the healthy store is already group/other readable\n' >&2
    exit 1
fi
chmod 0755 "$target/.ee"
chmod 0644 "$target/.ee/ee.db"

doctor_fixture_corrupt "$FM" "P1" "state_files"
exposed "$target/.ee/ee.db"
exposed "$target/.ee"
printf 'real exposure confirmed: .ee is 0755 and ee.db is 0644 (group/other readable)\n' >&2
