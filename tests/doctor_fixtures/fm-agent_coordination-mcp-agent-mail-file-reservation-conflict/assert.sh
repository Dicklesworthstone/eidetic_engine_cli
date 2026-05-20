#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
doctor_fixture_assert "fm-agent_coordination-mcp-agent-mail-file-reservation-conflict" "P1" "agent_coordination"
