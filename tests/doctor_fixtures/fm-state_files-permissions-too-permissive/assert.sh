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

# Independent witness: the store and its directory are group/other readable.
if ! exposed "$target/.ee/ee.db" || ! exposed "$target/.ee"; then
    printf 'fixture assert: %s witness lost: .ee or ee.db is no longer group/other readable\n' "$FM" >&2
    exit 1
fi
doctor_fixture_assert_pinned_gap "$FM" ".ee/ee.db"
# Nothing tightened the modes: the gap is still there after --fix.
exposed "$target/.ee/ee.db"
exposed "$target/.ee"
