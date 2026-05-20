#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
doctor_fixture_corrupt "fm-agent_coordination-rch-workers-all-blocked-by-pressure" "P0" "agent_coordination"
