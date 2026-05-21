#!/usr/bin/env bash
# Smoke-test the F1-F5 agent ergonomics e2e helper library.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

WORKSPACE="${WORKSPACE:-${TMPDIR:-/tmp}/ee-agent-ergonomics-smoke.${BASHPID:-$$}}"
LOG_DIR="${LOG_DIR:-$WORKSPACE/logs}"
EE_BIN="${EE_BIN:-ee}"
TEST_NAME="e2e_lib_smoke"
export WORKSPACE LOG_DIR EE_BIN TEST_NAME

mkdir -p "$WORKSPACE" "$LOG_DIR"

# shellcheck source=scripts/e2e_lib/agent_ergonomics_lib.sh
source "$REPO_ROOT/scripts/e2e_lib/agent_ergonomics_lib.sh"

payload='{"schema":"smoke.v1","success":true,"data":{"message":"ready"}}'
assert_jq "$payload" '.data.message' 'ready' 'jq assertion reads JSON'
assert_contains "$payload" '"success":true' 'contains assertion sees payload text'
