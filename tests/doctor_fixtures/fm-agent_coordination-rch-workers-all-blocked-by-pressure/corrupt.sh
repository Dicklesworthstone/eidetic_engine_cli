#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# NOT-DETECTED AS A FAILURE, PINNED GAP (bd-2oh15 c9853): the state lives in an
# EXTERNAL tool. A stand-in `rch` (a double for the external binary, not for ee
# internals) is placed first on PATH and reports its only worker blocked by
# critical disk pressure. ee parses that as healthy_but_pressure_blocked with
# zero usable workers, yet the rch_worker_pressure check stays severity ok.
FM="fm-agent_coordination-rch-workers-all-blocked-by-pressure"
target="$(doctor_fixture_target)"
ee_bin="$(command -v "${EE_DOCTOR_FIXTURE_BINARY:-ee}")"
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

mkdir -p "$base/fakebin"
printf '%s\n' '#!/bin/sh' \
    "printf '%s' '{\"workers\":[{\"id\":\"w1\",\"diskPressure\":\"critical\",\"freeGb\":0}]}'" \
    > "$base/fakebin/rch"
chmod +x "$base/fakebin/rch"

doctor_fixture_corrupt "$FM" "P1" "agent_coordination"
PATH="$base/fakebin:$PATH" "$ee_bin" doctor --workspace "$target" --full --json > "$base/doctor-corrupt.json"
if ! jq -e '.data.rchWorkerPressure.status == "healthy_but_pressure_blocked" and .data.rchWorkerPressure.usableWorkerCount == 0' \
    "$base/doctor-corrupt.json" >/dev/null; then
    printf 'rch-pressure fixture: ee did not parse all workers blocked; see %s\n' "$base/doctor-corrupt.json" >&2
    exit 1
fi
printf 'real environment confirmed: rch reports every worker blocked by disk pressure\n' >&2
