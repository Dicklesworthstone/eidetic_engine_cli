#!/usr/bin/env bash
# bd-1prrl.2.5 - per-agent context profile e2e evidence driver.
#
# This script intentionally retains its workspace and log artifacts. AGENTS.md
# forbids agent-side file deletion, and retained artifacts are useful evidence.

set -euo pipefail

require_jq() {
    if ! command -v jq >/dev/null 2>&1; then
        echo "sx5_agent_profile: jq is required" >&2
        exit 2
    fi
}

resolve_ee_binary() {
    if [ -n "${EE_BINARY:-}" ]; then
        printf '%s\n' "$EE_BINARY"
        return 0
    fi
    if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "${CARGO_TARGET_DIR%/}/debug/ee" ]; then
        printf '%s\n' "${CARGO_TARGET_DIR%/}/debug/ee"
        return 0
    fi
    if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "${CARGO_TARGET_DIR%/}/release/ee" ]; then
        printf '%s\n' "${CARGO_TARGET_DIR%/}/release/ee"
        return 0
    fi
    echo "sx5_agent_profile: set EE_BINARY or CARGO_TARGET_DIR to an ee binary" >&2
    exit 2
}

json_event() {
    local kind="${1:?kind required}"
    shift
    [ -z "${EE_TEST_LOG_PATH:-}" ] && return 0
    python3 - "$EE_TEST_LOG_PATH" "$kind" "$@" <<'PY'
import json
import os
import sys
from datetime import datetime, timezone

path = sys.argv[1]
event = {
    "schema": "ee.test_event.v1",
    "ts": datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "test_id": "sx5_agent_profile",
    "kind": sys.argv[2],
}
fields = {}
args = sys.argv[3:]
for index in range(0, len(args), 2):
    fields[args[index]] = args[index + 1]
event["fields"] = fields
os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
with open(path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(event, sort_keys=True) + "\n")
PY
}

assert_eq() {
    local got="${1:-}"
    local want="${2:-}"
    local label="${3:?label required}"
    if [ "$got" = "$want" ]; then
        ASSERTS_PASS=$((ASSERTS_PASS + 1))
        json_event "assert_ok" "label" "$label"
    else
        ASSERTS_FAIL=$((ASSERTS_FAIL + 1))
        json_event "assert_fail" "label" "$label" "expected" "$want" "actual" "$got"
    fi
}

run_ee_json() {
    local agent_name="${1:?agent required}"
    local label="${2:?label required}"
    shift 2
    json_event "command_start" "label" "$label" "agent" "$agent_name" "command" "$EE_BINARY $* --workspace $WORKSPACE"
    local output
    if output="$(env EE_AGENT_NAME="$agent_name" "$EE_BINARY" "$@" --workspace "$WORKSPACE" 2>&1)"; then
        json_event "command_end" "label" "$label" "agent" "$agent_name" "exit_code" "0"
        printf '%s\n' "$output"
        return 0
    fi
    local rc=$?
    json_event "command_end" "label" "$label" "agent" "$agent_name" "exit_code" "$rc" "stderr_excerpt" "$output"
    printf '%s\n' "$output" >&2
    exit "$rc"
}

record_outcomes() {
    local agent_name="${1:?agent required}"
    local memory_id="${2:?memory required}"
    local signal="${3:?signal required}"
    local label="${4:?label required}"
    local agent_seed
    local label_seed
    local signal_seed
    case "$agent_name" in
        "$AGENT_ALPHA") agent_seed=1 ;;
        "$AGENT_BETA") agent_seed=2 ;;
        *) agent_seed=9 ;;
    esac
    case "$label" in
        alpha) label_seed=1 ;;
        *) label_seed=2 ;;
    esac
    case "$signal" in
        helpful) signal_seed=1 ;;
        *) signal_seed=2 ;;
    esac
    local index
    for index in $(seq 0 9); do
        local event_seed
        event_seed=$((agent_seed * 1000 + label_seed * 100 + signal_seed * 10 + index))
        run_ee_json "$agent_name" "outcome_${agent_name}_${label}_${signal}_${index}" \
            outcome "$memory_id" \
            --signal "$signal" \
            --source-id "src_${agent_name}_${label}_${index}" \
            --event-id "$(printf 'fb_%026d' "$event_seed")" \
            --reason "${agent_name} ${signal} profile evidence for ${label} #${index}" \
            --harmful-per-source-per-hour 100 \
            --json >/dev/null
    done
}

require_jq
EE_BINARY="$(resolve_ee_binary)"
ASSERTS_PASS=0
ASSERTS_FAIL=0
TMP_ROOT="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
case "$TMP_ROOT" in
    /Volumes/*) TMP_ROOT="/tmp" ;;
esac
WORKSPACE="${TMP_ROOT%/}/ee-e2e-sx5-agent-profile.$$"
mkdir -p "$WORKSPACE"
export EE_TEST_LOG_PATH="${EE_TEST_LOG_PATH:-$WORKSPACE/sx5_agent_profile.jsonl}"

AGENT_ALPHA="AgentProfileAlpha"
AGENT_BETA="AgentProfileBeta"
AGENT_GAMMA="AgentProfileGamma"
QUERY="agent profile calibration sentinel"

json_event "note" "message" "sx5_agent_profile_start" "workspace" "$WORKSPACE" "bead_id" "bd-1prrl.2.5"

run_ee_json "$AGENT_ALPHA" "init" init --json >/dev/null

ALPHA_JSON="$(run_ee_json "$AGENT_ALPHA" "remember_alpha" remember --level procedural --kind rule "$QUERY: alpha preferred memory. This memory has identical retrieval terms." --json)"
BETA_JSON="$(run_ee_json "$AGENT_ALPHA" "remember_beta" remember --level procedural --kind rule "$QUERY: beta preferred memory. This memory has identical retrieval terms." --json)"
ALPHA_MEMORY="$(printf '%s' "$ALPHA_JSON" | jq -r '.data.memory_id // empty')"
BETA_MEMORY="$(printf '%s' "$BETA_JSON" | jq -r '.data.memory_id // empty')"
assert_eq "$(printf '%s' "$ALPHA_MEMORY" | grep -c '^mem_')" "1" "sx5_alpha_memory_id"
assert_eq "$(printf '%s' "$BETA_MEMORY" | grep -c '^mem_')" "1" "sx5_beta_memory_id"

record_outcomes "$AGENT_ALPHA" "$ALPHA_MEMORY" helpful alpha
record_outcomes "$AGENT_ALPHA" "$BETA_MEMORY" harmful beta
record_outcomes "$AGENT_BETA" "$ALPHA_MEMORY" harmful alpha
record_outcomes "$AGENT_BETA" "$BETA_MEMORY" helpful beta

ALPHA_CONTEXT="$(run_ee_json "$AGENT_ALPHA" "context_alpha" context "$QUERY" --max-tokens 1000 --explain --json)"
BETA_CONTEXT="$(run_ee_json "$AGENT_BETA" "context_beta" context "$QUERY" --max-tokens 1000 --explain --json)"
GAMMA_CONTEXT="$(run_ee_json "$AGENT_GAMMA" "context_gamma" context "$QUERY" --max-tokens 1000 --explain --json)"

ALPHA_FIRST="$(printf '%s' "$ALPHA_CONTEXT" | jq -r '.data.pack.items[0].memoryId // empty')"
BETA_FIRST="$(printf '%s' "$BETA_CONTEXT" | jq -r '.data.pack.items[0].memoryId // empty')"
GAMMA_PROFILE_PRESENT="$(printf '%s' "$GAMMA_CONTEXT" | jq -r 'has("data") and (.data.pack | has("agentProfile"))')"
ALPHA_BIAS="$(printf '%s' "$ALPHA_CONTEXT" | jq -r '.data.pack.agentProfile.biasMagnitude // empty')"
BETA_BIAS="$(printf '%s' "$BETA_CONTEXT" | jq -r '.data.pack.agentProfile.biasMagnitude // empty')"

assert_eq "$ALPHA_FIRST" "$ALPHA_MEMORY" "sx5_alpha_helpful_ranks_first"
assert_eq "$BETA_FIRST" "$BETA_MEMORY" "sx5_beta_helpful_ranks_first"
assert_eq "$GAMMA_PROFILE_PRESENT" "false" "sx5_third_agent_has_no_profile_leak"
assert_eq "$ALPHA_BIAS" "0.05" "sx5_alpha_bias_capped"
assert_eq "$BETA_BIAS" "0.05" "sx5_beta_bias_capped"

WHY_ALPHA="$(run_ee_json "$AGENT_ALPHA" "why_alpha" why "$ALPHA_MEMORY" --json)"
assert_eq "$(printf '%s' "$WHY_ALPHA" | jq -r '.data.agentProfile.helpfulCount // empty')" "10" "sx5_why_helpful_count"
assert_eq "$(printf '%s' "$WHY_ALPHA" | jq -r '.data.agentProfile.harmfulCount // empty')" "0" "sx5_why_harmful_count"

json_event "note" \
    "message" "sx5_agent_profile_summary" \
    "workspace" "$WORKSPACE" \
    "asserts_pass" "$ASSERTS_PASS" \
    "asserts_fail" "$ASSERTS_FAIL"

echo "sx5_agent_profile workspace retained: $WORKSPACE" >&2
echo "sx5_agent_profile log: $EE_TEST_LOG_PATH" >&2

if [ "$ASSERTS_FAIL" -gt 0 ]; then
    exit 1
fi
