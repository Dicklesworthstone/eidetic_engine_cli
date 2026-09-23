#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-agent_coordination-rch-workers-all-blocked-by-pressure"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"
EE_DOCTOR_FIXTURE_BINARY="$(command -v "${EE_DOCTOR_FIXTURE_BINARY:-ee}")"
export EE_DOCTOR_FIXTURE_BINARY
PATH="$base/fakebin:$PATH"
export PATH
ee_bin="$EE_DOCTOR_FIXTURE_BINARY"

# Witness 1: ee itself parses every worker as blocked, yet the check is ok.
"$ee_bin" doctor --workspace "$target" --full --json > "$base/doctor-full-assert.json"
jq -e '
    .data.rchWorkerPressure.status == "healthy_but_pressure_blocked" and
    .data.rchWorkerPressure.usableWorkerCount == 0 and
    any(.. | objects | select(.name? == "rch_worker_pressure"); .severity == "ok")
' "$base/doctor-full-assert.json" >/dev/null
# (Default `ee status` does not collect rch pressure at all -- its subsystem
# reports reason not_collected -- so it is not a witness here; bd-2oh15.)
doctor_fixture_assert_pinned_gap "$FM" "-"
