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

# The dueling-wizards feature e2e contract names scripts/e2e_lib.sh as the
# stable shared entrypoint. It is a thin compatibility layer over
# scripts/lib/e2e_harness.sh; smoke the named surface here so false-closures are
# caught before feature scripts depend on it.
export EE_TEST_LOG_PATH="$LOG_DIR/e2e_lib_events.jsonl"
export EE_E2E_KEEP="${EE_E2E_KEEP:-1}"

# shellcheck source=scripts/e2e_lib.sh
source "$REPO_ROOT/scripts/e2e_lib.sh"

harness_init "e2e_lib_smoke"
step "entrypoint compatibility"
log_event "note" "phase" "entrypoint" "detail" "scripts/e2e_lib.sh sourced"
assert_eq "ready" "ready" "assert_eq works"
assert_contains "$payload" '"success":true' "assert_contains works"
assert_jq "$payload" '.success == true and .data.message == "ready"' "assert_jq works"
assert_json "$payload" '.data.message' "ready" "assert_json extracts scalar"

step "isolated workspace"
smoke_ws=""
with_temp_workspace smoke_ws
assert_eq "$([ -d "$smoke_ws" ] && printf yes || printf no)" "yes" "workspace directory exists"
assert_eq "$([ -d "$smoke_ws/db" ] && printf yes || printf no)" "yes" "workspace db directory exists"
assert_eq "$([ -d "$smoke_ws/index" ] && printf yes || printf no)" "yes" "workspace index directory exists"
end_temp_workspace

summary
