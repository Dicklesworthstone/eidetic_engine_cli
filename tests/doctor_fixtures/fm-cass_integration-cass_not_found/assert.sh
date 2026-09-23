#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-cass_integration-cass_not_found"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"
EE_DOCTOR_FIXTURE_BINARY="$(command -v "${EE_DOCTOR_FIXTURE_BINARY:-ee}")"
export EE_DOCTOR_FIXTURE_BINARY
PATH="$(cat "$base/path-without-cass")"
export PATH
doctor_fixture_assert_report_only "$FM" "cass" "EE-E506" full "-"
